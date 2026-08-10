// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 mail component. The component owns view state and delegates all
// parsing and persistence to the framework-neutral MailStoreService.

use super::mail_detail::{MailDetailInput, MailDetailModel, MailDetailOutput};
use crate::models::mail_message::AttachmentInfo;
use crate::services::camel_mail_service;
use crate::services::eds_service::{self, EdsSource};
use crate::services::mail_store_service::{MailStoreService, ParsedMail};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

#[derive(Debug, Clone)]
pub(crate) struct MailSummary {
    pub(crate) id: String,
    pub(crate) folder_id: String,
    pub(crate) sender: String,
    pub(crate) sender_email: String,
    pub(crate) recipient_to: String,
    pub(crate) subject: String,
    pub(crate) date: String,
    pub(crate) preview: String,
    pub(crate) html_body: String,
    pub(crate) is_unread: bool,
    pub(crate) is_flagged: bool,
    pub(crate) attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailFolderFilter {
    All,
    Inbox,
    Sent,
    Drafts,
    Trash,
}

impl MailFolderFilter {
    fn matches(self, mail: &MailSummary) -> bool {
        match self {
            Self::All => true,
            Self::Inbox => mail.folder_id == "inbox",
            Self::Sent => mail.folder_id == "sent",
            Self::Drafts => mail.folder_id == "drafts",
            Self::Trash => mail.folder_id == "trash",
        }
    }
}

#[derive(Debug)]
struct MailSourceRow {
    source: EdsSource,
}

#[derive(Debug)]
enum MailSourceRowInput {}

#[relm4::factory]
impl FactoryComponent for MailSourceRow {
    type Init = EdsSource;
    type Input = MailSourceRowInput;
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 8,
            set_margin_start: 10,
            set_margin_end: 10,
            set_margin_top: 6,
            set_margin_bottom: 6,

            gtk::Image {
                set_icon_name: Some("mail-account-symbolic"),
                add_css_class: "dim-label",
            },
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 1,
                set_hexpand: true,

                gtk::Label {
                    #[watch]
                    set_label: &self.source.display_name,
                    set_halign: gtk::Align::Start,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                },
                gtk::Label {
                    #[watch]
                    set_label: &format!(
                        "{}{}",
                        self.source.backend_name,
                        if self.source.enabled { "" } else { " · deaktiviert" }
                    ),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
            },
        }
    }

    fn init_model(source: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { source }
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

#[derive(Debug)]
struct MailRow {
    summary: MailSummary,
}

#[derive(Debug)]
enum MailRowInput {
    Select,
}

#[derive(Debug)]
enum MailRowOutput {
    Selected(String),
}

#[relm4::factory]
impl FactoryComponent for MailRow {
    type Init = MailSummary;
    type Input = MailRowInput;
    type Output = MailRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        #[root]
        gtk::Button {
            set_has_frame: false,
            set_hexpand: true,
            set_halign: gtk::Align::Fill,
            connect_clicked => MailRowInput::Select,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 2,
                set_margin_start: 10,
                set_margin_end: 10,
                set_margin_top: 8,
                set_margin_bottom: 8,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,

                    gtk::Label {
                        #[watch]
                        set_label: &format!(
                            "{}{}",
                            if self.summary.is_unread { "● " } else { "" },
                            self.summary.subject
                        ),
                        set_halign: gtk::Align::Start,
                        set_hexpand: true,
                        set_wrap: true,
                    },
                    gtk::Image {
                        #[watch]
                        set_icon_name: Some(if self.summary.is_flagged {
                            "starred-symbolic"
                        } else {
                            ""
                        }),
                        set_halign: gtk::Align::End,
                    },
                },

                gtk::Label {
                    #[watch]
                    set_label: &format!("{} · {}", self.summary.sender, self.summary.date),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },

                gtk::Label {
                    #[watch]
                    set_label: &self.summary.preview,
                    set_halign: gtk::Align::Start,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_lines: 1,
                    add_css_class: "dim-label",
                },
            },
        }
    }

    fn init_model(
        summary: Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        Self { summary }
    }

    fn update(&mut self, message: Self::Input, sender: FactorySender<Self>) {
        match message {
            MailRowInput::Select => {
                let _ = sender.output(MailRowOutput::Selected(self.summary.id.clone()));
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum MailInput {
    SearchChanged(String),
    Refresh,
    Select(String),
    FolderSelected(MailFolderFilter),
    DetailStateChanged {
        id: String,
        is_unread: bool,
        is_flagged: bool,
    },
}

#[derive(Debug)]
pub enum MailCommand {
    Loaded {
        messages: Vec<MailSummary>,
        status: String,
    },
    SourcesLoaded {
        sources: Vec<EdsSource>,
        status: String,
    },
}

#[derive(Debug)]
pub(crate) struct MailModel {
    search_query: String,
    status: String,
    mail_status: String,
    source_status: String,
    loading: bool,
    all_mail: Vec<MailSummary>,
    folder_filter: MailFolderFilter,
    selected_id: Option<String>,
    rows: FactoryVecDeque<MailRow>,
    source_rows: FactoryVecDeque<MailSourceRow>,
    detail: relm4::Controller<MailDetailModel>,
}

impl MailModel {
    fn refresh_visible_rows(&mut self) {
        let query = self.search_query.to_lowercase();
        let visible = self
            .all_mail
            .iter()
            .filter(|mail| {
                self.folder_filter.matches(mail)
                    && (query.is_empty()
                        || mail.subject.to_lowercase().contains(&query)
                        || mail.sender.to_lowercase().contains(&query))
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut rows = self.rows.guard();
        rows.clear();
        for summary in visible {
            rows.push_back(summary);
        }
    }

    fn show_selected(&self) {
        let selected = self
            .selected_id
            .as_ref()
            .and_then(|id| self.all_mail.iter().find(|mail| &mail.id == id))
            .cloned();
        let _ = self.detail.sender().send(MailDetailInput::Show(selected));
    }
}

#[relm4::component(pub)]
impl Component for MailModel {
    type Init = ();
    type Input = MailInput;
    type Output = ();
    type CommandOutput = MailCommand;

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 8,
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_top: 10,
                set_margin_bottom: 10,

                gtk::SearchEntry {
                    set_hexpand: true,
                    set_placeholder_text: Some("Mail durchsuchen…"),
                    connect_search_changed[sender] => move |entry| {
                        sender.input(MailInput::SearchChanged(entry.text().to_string()));
                    },
                },

                gtk::Button {
                    set_label: "Aktualisieren",
                    connect_clicked => MailInput::Refresh,
                },
            },

            #[name(content_box)]
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_vexpand: true,
                set_hexpand: true,
            },

            gtk::Label {
                #[watch]
                set_label: &model.status,
                set_halign: gtk::Align::Start,
                add_css_class: "dim-label",
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_top: 6,
                set_margin_bottom: 8,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = FactoryVecDeque::builder()
            .launch(
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build(),
            )
            .forward(sender.input_sender(), |output| match output {
                MailRowOutput::Selected(id) => MailInput::Select(id),
            });
        let source_rows = FactoryVecDeque::builder()
            .launch(
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build(),
            )
            .detach();
        let detail =
            MailDetailModel::builder()
                .launch(())
                .forward(sender.input_sender(), |output| match output {
                    MailDetailOutput::StateChanged {
                        id,
                        is_unread,
                        is_flagged,
                    } => MailInput::DetailStateChanged {
                        id,
                        is_unread,
                        is_flagged,
                    },
                });
        let model = MailModel {
            search_query: String::new(),
            status: "Mail wird geladen…".to_string(),
            mail_status: "EDS/Camel wird initialisiert…".to_string(),
            source_status: "EDS-Konten werden geladen…".to_string(),
            loading: false,
            all_mail: Vec::new(),
            folder_filter: MailFolderFilter::All,
            selected_id: None,
            rows,
            source_rows,
            detail,
        };
        let row_box = model.rows.widget();
        let source_row_box = model.source_rows.widget();
        let detail_widget = model.detail.widget().clone();
        let widgets = view_output!();

        let sidebar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(240)
            .build();
        sidebar.append(
            &gtk::Label::builder()
                .label("EDS-Konten")
                .margin_start(10)
                .margin_top(10)
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .build(),
        );
        sidebar.append(source_row_box);
        sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let folder_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        for title in [
            "Alle Nachrichten",
            "Posteingang",
            "Gesendet",
            "Entwürfe",
            "Papierkorb",
        ] {
            folder_list.append(
                &gtk::Label::builder()
                    .label(title)
                    .margin_top(8)
                    .margin_bottom(8)
                    .margin_start(10)
                    .margin_end(10)
                    .halign(gtk::Align::Start)
                    .build(),
            );
        }
        let folder_sender = sender.clone();
        folder_list.connect_row_selected(move |_, row| {
            let filter = match row.map(|row| row.index()) {
                Some(1) => MailFolderFilter::Inbox,
                Some(2) => MailFolderFilter::Sent,
                Some(3) => MailFolderFilter::Drafts,
                Some(4) => MailFolderFilter::Trash,
                _ => MailFolderFilter::All,
            };
            folder_sender.input(MailInput::FolderSelected(filter));
        });
        sidebar.append(&folder_list);
        widgets.content_box.append(&sidebar);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(420)
            .vexpand(true)
            .hexpand(true)
            .child(row_box)
            .build();
        widgets.content_box.append(&scroll);

        let detail_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .hexpand(true)
            .child(&detail_widget)
            .build();
        widgets.content_box.append(&detail_scroll);

        sender.spawn_oneshot_command(|| load_mail());
        sender.oneshot_command(async { load_mail_sources().await });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            MailInput::SearchChanged(query) => {
                self.search_query = query;
                self.refresh_visible_rows();
                self.status = format!("{} Treffer · {}", self.rows.len(), self.mail_status);
            }
            MailInput::FolderSelected(filter) => {
                self.folder_filter = filter;
                self.refresh_visible_rows();
                self.status = format!("{} Treffer · {}", self.rows.len(), self.mail_status);
            }
            MailInput::Refresh => {
                if !self.loading {
                    self.loading = true;
                    self.status = "Mail wird geladen…".to_string();
                    sender.spawn_oneshot_command(|| load_mail());
                }
                sender.oneshot_command(async { load_mail_sources().await });
            }
            MailInput::Select(id) => {
                if self.all_mail.iter().any(|mail| mail.id == id) {
                    self.selected_id = Some(id);
                    self.show_selected();
                }
            }
            MailInput::DetailStateChanged {
                id,
                is_unread,
                is_flagged,
            } => {
                if let Some(message) = self.all_mail.iter_mut().find(|mail| mail.id == id) {
                    message.is_unread = is_unread;
                    message.is_flagged = is_flagged;
                }
                self.refresh_visible_rows();
                self.show_selected();
            }
        }
    }

    fn update_cmd(
        &mut self,
        message: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            MailCommand::Loaded { messages, status } => {
                self.loading = false;
                self.all_mail = messages;
                self.mail_status = status;
                self.refresh_visible_rows();
                let count = self.rows.len();
                if self
                    .selected_id
                    .as_ref()
                    .is_none_or(|id| !self.all_mail.iter().any(|mail| &mail.id == id))
                {
                    self.selected_id = self
                        .all_mail
                        .iter()
                        .find(|mail| {
                            let query = self.search_query.to_lowercase();
                            query.is_empty()
                                || mail.subject.to_lowercase().contains(&query)
                                || mail.sender.to_lowercase().contains(&query)
                        })
                        .map(|mail| mail.id.clone());
                }
                self.show_selected();
                self.status = if count == 0 {
                    self.mail_status.clone()
                } else {
                    format!("{count} sichtbare Nachricht(en) · {}", self.mail_status)
                };
            }
            MailCommand::SourcesLoaded { sources, status } => {
                self.source_status = status;
                let mut rows = self.source_rows.guard();
                rows.clear();
                for source in sources {
                    rows.push_back(source);
                }
                if !self.all_mail.is_empty() {
                    self.status = format!(
                        "{} sichtbare Nachricht(en) · {}",
                        self.rows.len(),
                        self.mail_status
                    );
                }
            }
        }
    }
}

async fn load_mail_sources() -> MailCommand {
    let discovery = eds_service::discover_sources(eds_service::EdsSourceKind::Mail).await;
    let status = discovery.status_text();
    MailCommand::SourcesLoaded {
        sources: discovery.sources,
        status,
    }
}

fn load_mail() -> MailCommand {
    match camel_mail_service::load_messages() {
        Ok(camel) if camel.configured_sources > 0 => {
            let status = format_camel_status(&camel);
            return MailCommand::Loaded {
                messages: summaries(camel.messages),
                status,
            };
        }
        Ok(camel) if !camel.messages.is_empty() => {
            let status = format_camel_status(&camel);
            return MailCommand::Loaded {
                messages: summaries(camel.messages),
                status,
            };
        }
        Ok(_) => {
            return load_local_mail(Some(
                "Keine aktiven EDS-Mailkonten; lokaler Evolution-Speicher",
            ));
        }
        Err(error) => {
            return load_local_mail(Some(&format!(
                "EDS/Camel nicht verfügbar ({error}); lokaler Evolution-Speicher"
            )));
        }
    }
}

fn load_local_mail(reason: Option<&str>) -> MailCommand {
    let messages = MailStoreService::new().load_local_records();
    let count = messages.len();
    MailCommand::Loaded {
        messages: summaries(messages),
        status: format!(
            "{} · {count} lokale Nachricht(en) geladen",
            reason.unwrap_or("Lokaler Evolution-Speicher")
        ),
    }
}

fn format_camel_status(camel: &camel_mail_service::CamelMailLoad) -> String {
    let account_label = if camel.configured_sources == 1 {
        "Konto"
    } else {
        "Konten"
    };
    let message_label = if camel.messages.len() == 1 {
        "Nachricht"
    } else {
        "Nachrichten"
    };
    let warning = camel
        .warning
        .as_deref()
        .map(|warning| format!(" · Hinweis: {warning}"))
        .unwrap_or_default();

    format!(
        "EDS/Camel: {} {account_label}, {} {message_label} geladen{warning}",
        camel.configured_sources,
        camel.messages.len()
    )
}

fn summaries(messages: Vec<ParsedMail>) -> Vec<MailSummary> {
    messages
        .into_iter()
        .map(|mail| MailSummary {
            id: mail.id,
            folder_id: mail.folder_id,
            sender: if mail.sender_name.is_empty() {
                mail.sender_email.clone()
            } else {
                mail.sender_name.clone()
            },
            sender_email: mail.sender_email,
            recipient_to: mail.recipient_to,
            subject: if mail.subject.is_empty() {
                "(Ohne Betreff)".to_string()
            } else {
                mail.subject
            },
            date: mail.date_str,
            preview: mail.preview_snippet,
            html_body: mail.html_body,
            is_unread: mail.is_unread,
            is_flagged: mail.is_flagged,
            attachments: mail.attachments,
        })
        .collect()
}
