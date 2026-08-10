// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 mail component. The component owns view state and delegates all
// parsing and persistence to the framework-neutral MailStoreService.

use super::mail_detail::{MailDetailInput, MailDetailModel, MailDetailOutput};
use crate::models::mail_message::AttachmentInfo;
use crate::services::camel_mail_service;
use crate::services::eds_service::{self, EdsSource};
use crate::services::mail_store_service::{MailBackendLocation, MailStoreService, ParsedMail};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;
use std::collections::HashMap;

const DEFAULT_MAIL_PAGE_SIZE: i32 = 100;

#[derive(Debug, Clone)]
pub(crate) struct MailSummary {
    pub(crate) id: String,
    pub(crate) folder_id: String,
    pub(crate) folder_name: String,
    pub(crate) backend_location: Option<MailBackendLocation>,
    pub(crate) sender: String,
    pub(crate) sender_email: String,
    pub(crate) recipient_to: String,
    pub(crate) subject: String,
    pub(crate) date: String,
    pub(crate) preview: String,
    pub(crate) html_body: String,
    pub(crate) is_unread: bool,
    pub(crate) is_flagged: bool,
    pub(crate) is_batch_selected: bool,
    pub(crate) attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MailFolderFilter {
    All,
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    Named {
        source_uid: String,
        folder_name: String,
    },
}

impl MailFolderFilter {
    fn matches(&self, mail: &MailSummary) -> bool {
        match self {
            Self::All => true,
            Self::Inbox => mail.folder_id == "inbox",
            Self::Sent => mail.folder_id == "sent",
            Self::Drafts => mail.folder_id == "drafts",
            Self::Trash => mail.folder_id == "trash",
            Self::Junk => mail.folder_id == "junk",
            Self::Archive => mail.folder_id == "archive",
            Self::Named {
                source_uid,
                folder_name,
            } => mail.backend_location.as_ref().is_some_and(|location| {
                &location.source_uid == source_uid && &location.folder_name == folder_name
            }),
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
struct MailFolderRow {
    folder: camel_mail_service::CamelFolderSummary,
}

#[derive(Debug)]
enum MailFolderRowInput {
    Select,
}

#[derive(Debug)]
enum MailFolderRowOutput {
    Selected(MailFolderFilter),
}

#[relm4::factory]
impl FactoryComponent for MailFolderRow {
    type Init = camel_mail_service::CamelFolderSummary;
    type Input = MailFolderRowInput;
    type Output = MailFolderRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        #[root]
        gtk::Button {
            set_has_frame: false,
            set_sensitive: self.folder.is_selectable,
            set_halign: gtk::Align::Fill,
            set_hexpand: true,
            connect_clicked => MailFolderRowInput::Select,

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 6,
                set_margin_top: 5,
                set_margin_bottom: 5,
                set_margin_start: 10,
                set_margin_end: 8,

                gtk::Image {
                    #[watch]
                    set_icon_name: Some(match self.folder.folder_id.as_str() {
                        "inbox" => "mail-inbox-symbolic",
                        "sent" => "mail-send-symbolic",
                        "drafts" => "text-x-generic-symbolic",
                        "trash" => "user-trash-symbolic",
                        "junk" => "mail-mark-junk-symbolic",
                        "archive" => "mail-archive-symbolic",
                        _ => "folder-symbolic",
                    }),
                },
                gtk::Label {
                    #[watch]
                    set_label: &self.folder.display_name,
                    set_halign: gtk::Align::Start,
                    set_hexpand: true,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                },
                gtk::Label {
                    #[watch]
                    set_label: &if self.folder.total_count > 0 {
                        if self.folder.unread_count > 0 {
                            format!(
                                "{}/{}",
                                self.folder.unread_count,
                                self.folder.total_count
                            )
                        } else {
                            self.folder.total_count.to_string()
                        }
                    } else {
                        String::new()
                    },
                    set_halign: gtk::Align::End,
                    add_css_class: "dim-label",
                },
            },
        }
    }

    fn init_model(folder: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { folder }
    }

    fn update(&mut self, message: Self::Input, sender: FactorySender<Self>) {
        if matches!(message, MailFolderRowInput::Select) {
            let _ = sender.output(MailFolderRowOutput::Selected(MailFolderFilter::Named {
                source_uid: self.folder.source_uid.clone(),
                folder_name: self.folder.folder_name.clone(),
            }));
        }
    }
}

#[derive(Debug)]
struct MailRow {
    summary: MailSummary,
}

#[derive(Debug)]
enum MailRowInput {
    Select,
    BatchSelectionChanged(bool),
}

#[derive(Debug)]
enum MailRowOutput {
    Selected(String),
    BatchSelectionChanged { id: String, selected: bool },
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
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 4,
            set_hexpand: true,
            set_halign: gtk::Align::Fill,

            gtk::CheckButton {
                #[watch]
                set_active: self.summary.is_batch_selected,
                set_valign: gtk::Align::Start,
                set_margin_start: 8,
                set_margin_top: 10,
                connect_toggled[sender] => move |button| {
                    sender.input(MailRowInput::BatchSelectionChanged(button.is_active()));
                },
            },

            gtk::Button {
                set_has_frame: false,
                set_hexpand: true,
                set_halign: gtk::Align::Fill,
                connect_clicked => MailRowInput::Select,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 2,
                    set_margin_start: 4,
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
            MailRowInput::BatchSelectionChanged(selected) => {
                self.summary.is_batch_selected = selected;
                let _ = sender.output(MailRowOutput::BatchSelectionChanged {
                    id: self.summary.id.clone(),
                    selected,
                });
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum MailInput {
    SearchChanged(String),
    Refresh,
    LoadMore,
    BatchAction(MailBatchAction),
    BatchSelectionChanged {
        id: String,
        selected: bool,
    },
    MoveSelectedTo(String),
    Select(String),
    FolderSelected(MailFolderFilter),
    DetailStateChanged {
        id: String,
        is_unread: bool,
        is_flagged: bool,
    },
    MessageRemoved {
        id: String,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MailBatchAction {
    Archive,
    Delete,
    SetState {
        is_unread: Option<bool>,
        is_flagged: Option<bool>,
    },
}

impl MailBatchAction {
    fn next_state(self, current_is_unread: bool, current_is_flagged: bool) -> Option<(bool, bool)> {
        match self {
            Self::SetState {
                is_unread,
                is_flagged,
            } => Some((
                is_unread.unwrap_or(current_is_unread),
                is_flagged.unwrap_or(current_is_flagged),
            )),
            Self::Archive | Self::Delete => None,
        }
    }

    fn status_label(self) -> &'static str {
        match self {
            Self::Archive => "archiviert",
            Self::Delete => "gelöscht",
            Self::SetState {
                is_unread: Some(true),
                is_flagged: None,
            } => "als ungelesen markiert",
            Self::SetState {
                is_unread: Some(false),
                is_flagged: None,
            } => "als gelesen markiert",
            Self::SetState {
                is_unread: None,
                is_flagged: Some(true),
            } => "markiert",
            Self::SetState {
                is_unread: None,
                is_flagged: Some(false),
            } => "Markierungen entfernt",
            Self::SetState { .. } => "Status aktualisiert",
        }
    }
}

#[derive(Debug)]
pub enum MailCommand {
    Loaded {
        messages: Vec<MailSummary>,
        folders: Vec<camel_mail_service::CamelFolderSummary>,
        status: String,
        append: bool,
        can_load_more: bool,
    },
    BatchFinished {
        action: MailBatchAction,
        succeeded: Vec<String>,
        failed: Vec<(String, String)>,
    },
    MoveFinished {
        id: String,
        destination: String,
        result: Result<(), String>,
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
    page_offset: usize,
    page_size: i32,
    can_load_more: bool,
    all_mail: Vec<MailSummary>,
    batch_selected: std::collections::HashSet<String>,
    batch_popover: gtk::Popover,
    move_folders: Vec<camel_mail_service::CamelFolderSummary>,
    move_list: gtk::ListBox,
    move_popover: gtk::Popover,
    folder_filter: MailFolderFilter,
    selected_id: Option<String>,
    rows: FactoryVecDeque<MailRow>,
    source_rows: FactoryVecDeque<MailSourceRow>,
    folder_rows: FactoryVecDeque<MailFolderRow>,
    detail: relm4::Controller<MailDetailModel>,
}

impl MailModel {
    fn can_move_selected(&self) -> bool {
        self.selected_id
            .as_ref()
            .and_then(|id| self.all_mail.iter().find(|mail| &mail.id == id))
            .and_then(|mail| mail.backend_location.as_ref())
            .is_some()
    }

    fn rebuild_move_menu(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.move_list.first_child() {
            self.move_list.remove(&child);
        }

        let selected_location = self
            .selected_id
            .as_ref()
            .and_then(|id| self.all_mail.iter().find(|mail| &mail.id == id))
            .and_then(|mail| mail.backend_location.as_ref());
        let Some(selected_location) = selected_location else {
            return;
        };

        let targets = self
            .move_folders
            .iter()
            .filter(|folder| {
                folder.is_selectable
                    && folder.source_uid == selected_location.source_uid
                    && folder.folder_name != selected_location.folder_name
            })
            .map(|folder| (folder.display_name.clone(), folder.folder_name.clone()))
            .collect::<Vec<_>>();

        if targets.is_empty() {
            let label = gtk::Label::builder()
                .label("Keine anderen EDS-Ordner verfügbar")
                .margin_start(12)
                .margin_end(12)
                .margin_top(10)
                .margin_bottom(10)
                .build();
            self.move_list.append(&label);
            return;
        }

        for (display_name, destination) in targets {
            let button = gtk::Button::builder()
                .label(display_name)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .build();
            button.set_has_frame(false);
            let input_sender = sender.clone();
            button.connect_clicked(move |_| {
                input_sender.input(MailInput::MoveSelectedTo(destination.clone()));
            });
            self.move_list.append(&button);
        }
    }

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
                gtk::Button {
                    set_label: "Mehr laden",
                    #[watch]
                    set_sensitive: !model.loading && model.can_load_more,
                    connect_clicked => MailInput::LoadMore,
                },
                gtk::Button {
                    set_label: "Auswahl archivieren",
                    #[watch]
                    set_sensitive: !model.loading && !model.batch_selected.is_empty(),
                    connect_clicked => MailInput::BatchAction(MailBatchAction::Archive),
                },
                gtk::Button {
                    set_label: "Auswahl löschen",
                    #[watch]
                    set_sensitive: !model.loading && !model.batch_selected.is_empty(),
                    connect_clicked => MailInput::BatchAction(MailBatchAction::Delete),
                },
                #[name(batch_button)]
                gtk::MenuButton {
                    set_label: "Weitere Aktionen…",
                    set_tooltip_text: Some("Weitere Aktionen für die Auswahl"),
                    #[watch]
                    set_sensitive: !model.loading && !model.batch_selected.is_empty(),
                },
                #[name(move_button)]
                gtk::MenuButton {
                    set_label: "Verschieben…",
                    set_tooltip_text: Some("Ausgewählte Nachricht verschieben"),
                    #[watch]
                    set_sensitive: !model.loading && model.can_move_selected(),
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
                MailRowOutput::BatchSelectionChanged { id, selected } => {
                    MailInput::BatchSelectionChanged { id, selected }
                }
            });
        let source_rows = FactoryVecDeque::builder()
            .launch(
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build(),
            )
            .detach();
        let folder_rows = FactoryVecDeque::builder()
            .launch(
                gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build(),
            )
            .forward(sender.input_sender(), |output| match output {
                MailFolderRowOutput::Selected(filter) => MailInput::FolderSelected(filter),
            });
        let batch_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        for (label, action) in [
            (
                "Als gelesen markieren",
                MailBatchAction::SetState {
                    is_unread: Some(false),
                    is_flagged: None,
                },
            ),
            (
                "Als ungelesen markieren",
                MailBatchAction::SetState {
                    is_unread: Some(true),
                    is_flagged: None,
                },
            ),
            (
                "Markieren",
                MailBatchAction::SetState {
                    is_unread: None,
                    is_flagged: Some(true),
                },
            ),
            (
                "Markierung entfernen",
                MailBatchAction::SetState {
                    is_unread: None,
                    is_flagged: Some(false),
                },
            ),
        ] {
            let button = gtk::Button::builder()
                .label(label)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .build();
            button.set_has_frame(false);
            let input_sender = sender.clone();
            button.connect_clicked(move |_| {
                input_sender.input(MailInput::BatchAction(action));
            });
            batch_list.append(&button);
        }
        let batch_popover = gtk::Popover::builder().child(&batch_list).build();
        let move_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        let move_popover = gtk::Popover::builder().child(&move_list).build();
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
                    MailDetailOutput::MessageRemoved { id } => MailInput::MessageRemoved { id },
                });
        let page_size = camel_mail_service::default_page_limit().max(DEFAULT_MAIL_PAGE_SIZE);
        let model = MailModel {
            search_query: String::new(),
            status: "Mail wird geladen…".to_string(),
            mail_status: "EDS/Camel wird initialisiert…".to_string(),
            source_status: "EDS-Konten werden geladen…".to_string(),
            loading: false,
            page_offset: 0,
            page_size,
            can_load_more: false,
            all_mail: Vec::new(),
            batch_selected: std::collections::HashSet::new(),
            batch_popover: batch_popover.clone(),
            move_folders: Vec::new(),
            move_list: move_list.clone(),
            move_popover: move_popover.clone(),
            folder_filter: MailFolderFilter::All,
            selected_id: None,
            rows,
            source_rows,
            folder_rows,
            detail,
        };
        let row_box = model.rows.widget();
        let source_row_box = model.source_rows.widget();
        let folder_row_box = model.folder_rows.widget();
        let detail_widget = model.detail.widget().clone();
        let widgets = view_output!();
        widgets.batch_button.set_popover(Some(&batch_popover));
        widgets.move_button.set_popover(Some(&move_popover));

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
            "Spam",
            "Archiv",
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
                Some(5) => MailFolderFilter::Junk,
                Some(6) => MailFolderFilter::Archive,
                _ => MailFolderFilter::All,
            };
            folder_sender.input(MailInput::FolderSelected(filter));
        });
        sidebar.append(&folder_list);
        sidebar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar.append(folder_row_box);
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

        sender.spawn_oneshot_command(move || load_mail_page(0, page_size, false));
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
                    self.page_offset = 0;
                    self.can_load_more = false;
                    self.status = "Mail wird geladen…".to_string();
                    let page_size = self.page_size;
                    sender.spawn_oneshot_command(move || load_mail_page(0, page_size, false));
                }
                sender.oneshot_command(async { load_mail_sources().await });
            }
            MailInput::LoadMore => {
                if !self.loading && self.can_load_more {
                    self.loading = true;
                    self.status = "Weitere Nachrichten werden geladen…".to_string();
                    let offset = self.page_offset;
                    let page_size = self.page_size;
                    sender.spawn_oneshot_command(move || load_mail_page(offset, page_size, true));
                }
            }
            MailInput::BatchSelectionChanged { id, selected } => {
                if selected {
                    self.batch_selected.insert(id.clone());
                } else {
                    self.batch_selected.remove(&id);
                }
                if let Some(message) = self.all_mail.iter_mut().find(|mail| mail.id == id) {
                    message.is_batch_selected = selected;
                }
            }
            MailInput::BatchAction(action) => {
                if self.loading || self.batch_selected.is_empty() {
                    return;
                }
                let requests = self
                    .all_mail
                    .iter()
                    .filter(|mail| self.batch_selected.contains(&mail.id))
                    .map(|mail| MailBatchRequest {
                        id: mail.id.clone(),
                        location: mail.backend_location.clone(),
                        is_unread: mail.is_unread,
                        is_flagged: mail.is_flagged,
                    })
                    .collect::<Vec<_>>();
                if requests.is_empty() {
                    return;
                }
                self.loading = true;
                self.batch_popover.popdown();
                self.status = match action {
                    MailBatchAction::Archive => "Auswahl wird archiviert…".to_string(),
                    MailBatchAction::Delete => "Auswahl wird gelöscht…".to_string(),
                    MailBatchAction::SetState {
                        is_unread: Some(true),
                        is_flagged: None,
                    } => "Auswahl wird als ungelesen markiert…".to_string(),
                    MailBatchAction::SetState {
                        is_unread: Some(false),
                        is_flagged: None,
                    } => "Auswahl wird als gelesen markiert…".to_string(),
                    MailBatchAction::SetState {
                        is_unread: None,
                        is_flagged: Some(true),
                    } => "Auswahl wird markiert…".to_string(),
                    MailBatchAction::SetState {
                        is_unread: None,
                        is_flagged: Some(false),
                    } => "Markierungen der Auswahl werden entfernt…".to_string(),
                    MailBatchAction::SetState { .. } => {
                        "Auswahlstatus wird aktualisiert…".to_string()
                    }
                };
                sender.spawn_oneshot_command(move || apply_batch(action, requests));
            }
            MailInput::MoveSelectedTo(destination) => {
                if self.loading {
                    return;
                }
                let Some(id) = self.selected_id.clone() else {
                    return;
                };
                let Some(message) = self.all_mail.iter().find(|mail| mail.id == id) else {
                    return;
                };
                let Some(location) = message.backend_location.clone() else {
                    self.move_popover.popdown();
                    self.status =
                        "Verschieben ist für lokale Nachrichten nicht verfügbar".to_string();
                    return;
                };
                if location.folder_name == destination {
                    self.move_popover.popdown();
                    self.status = "Nachricht befindet sich bereits in diesem Ordner".to_string();
                    return;
                }
                self.loading = true;
                self.move_popover.popdown();
                self.status = "Nachricht wird verschoben…".to_string();
                sender.spawn_oneshot_command(move || apply_move(id, location, destination));
            }
            MailInput::Select(id) => {
                if self.all_mail.iter().any(|mail| mail.id == id) {
                    self.selected_id = Some(id);
                    self.rebuild_move_menu(&sender);
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
            MailInput::MessageRemoved { id } => {
                self.all_mail.retain(|mail| mail.id != id);
                if self.selected_id.as_deref() == Some(id.as_str()) {
                    self.selected_id = None;
                }
                self.rebuild_move_menu(&sender);
                self.refresh_visible_rows();
                self.show_selected();
                self.status = format!(
                    "{} sichtbare Nachricht(en) · {}",
                    self.rows.len(),
                    self.mail_status
                );
            }
        }
    }

    fn update_cmd(
        &mut self,
        message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            MailCommand::Loaded {
                messages,
                folders,
                status,
                append,
                can_load_more,
            } => {
                self.loading = false;
                if append {
                    let existing_ids = self
                        .all_mail
                        .iter()
                        .map(|mail| mail.id.clone())
                        .collect::<std::collections::HashSet<_>>();
                    self.all_mail.extend(
                        messages
                            .into_iter()
                            .filter(|mail| !existing_ids.contains(&mail.id)),
                    );
                } else {
                    self.all_mail = messages;
                }
                self.mail_status = status;
                self.can_load_more = can_load_more;
                self.page_offset = self.page_offset.saturating_add(self.page_size as usize);
                if !append || !folders.is_empty() {
                    self.move_folders = folders.clone();
                    self.rebuild_move_menu(&sender);
                    {
                        let mut folder_rows = self.folder_rows.guard();
                        folder_rows.clear();
                        for folder in folders {
                            folder_rows.push_back(folder);
                        }
                    }
                }
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
            MailCommand::BatchFinished {
                action,
                succeeded,
                failed,
            } => {
                self.loading = false;
                let succeeded_count = succeeded.len();
                let succeeded_ids = succeeded
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>();
                let removes_messages =
                    matches!(action, MailBatchAction::Archive | MailBatchAction::Delete);
                if removes_messages {
                    self.all_mail
                        .retain(|mail| !succeeded_ids.contains(&mail.id));
                    self.batch_selected.retain(|id| !succeeded_ids.contains(id));
                }
                for message in &mut self.all_mail {
                    if succeeded_ids.contains(&message.id) {
                        if let Some((is_unread, is_flagged)) =
                            action.next_state(message.is_unread, message.is_flagged)
                        {
                            message.is_unread = is_unread;
                            message.is_flagged = is_flagged;
                        }
                    }
                    message.is_batch_selected = self.batch_selected.contains(&message.id);
                }
                if removes_messages
                    && self
                        .selected_id
                        .as_ref()
                        .is_some_and(|id| succeeded_ids.contains(id))
                {
                    self.selected_id = None;
                }
                self.refresh_visible_rows();
                self.show_selected();
                let action_label = action.status_label();
                self.status = if failed.is_empty() {
                    format!("{succeeded_count} Nachricht(en) {action_label}")
                } else {
                    format!(
                        "{succeeded_count} Nachricht(en) {action_label}; {} fehlgeschlagen",
                        failed.len()
                    )
                };
            }
            MailCommand::MoveFinished {
                id,
                destination,
                result,
            } => {
                self.loading = false;
                match result {
                    Ok(()) => {
                        self.all_mail.retain(|mail| mail.id != id);
                        self.batch_selected.remove(&id);
                        if self.selected_id.as_deref() == Some(id.as_str()) {
                            self.selected_id = None;
                        }
                        self.rebuild_move_menu(&sender);
                        self.refresh_visible_rows();
                        self.show_selected();
                        self.status = format!(
                            "Nachricht nach {destination} verschoben · Ordnerzähler mit Aktualisieren erneuern"
                        );
                    }
                    Err(error) => {
                        self.status = format!("Verschieben fehlgeschlagen: {error}");
                    }
                }
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

struct MailBatchRequest {
    id: String,
    location: Option<MailBackendLocation>,
    is_unread: bool,
    is_flagged: bool,
}

fn apply_batch(action: MailBatchAction, requests: Vec<MailBatchRequest>) -> MailCommand {
    let operations = crate::services::mail_operations_service::MailOperationsService::new();
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();

    for request in requests {
        let result = match action {
            MailBatchAction::Archive => match request.location.as_ref() {
                Some(location) => operations.archive_for_location(&request.id, location),
                None => operations.archive(&request.id),
            },
            MailBatchAction::Delete => match request.location.as_ref() {
                Some(location) => operations.delete_for_location(&request.id, location),
                None => operations.delete(&request.id),
            },
            MailBatchAction::SetState {
                is_unread,
                is_flagged,
            } => {
                let next_is_unread = is_unread.unwrap_or(request.is_unread);
                let next_is_flagged = is_flagged.unwrap_or(request.is_flagged);
                match request.location.as_ref() {
                    Some(location) => operations.set_message_state_for_location(
                        &request.id,
                        location,
                        next_is_unread,
                        next_is_flagged,
                    ),
                    None => {
                        operations.set_message_state(&request.id, next_is_unread, next_is_flagged)
                    }
                }
            }
        };

        match result {
            Ok(()) => succeeded.push(request.id),
            Err(error) => failed.push((request.id, error.to_string())),
        }
    }

    MailCommand::BatchFinished {
        action,
        succeeded,
        failed,
    }
}

fn apply_move(id: String, location: MailBackendLocation, destination: String) -> MailCommand {
    let result = crate::services::mail_operations_service::MailOperationsService::new()
        .move_to_for_location(&id, &location, &destination)
        .map_err(|error| error.to_string());

    MailCommand::MoveFinished {
        id,
        destination,
        result,
    }
}

fn load_mail_page(message_offset: usize, page_size: i32, append: bool) -> MailCommand {
    match camel_mail_service::load_messages_page(message_offset, page_size) {
        Ok(camel) if camel.configured_sources > 0 => {
            let status = format_camel_status(&camel);
            return loaded_camel_page(camel, status, message_offset, append);
        }
        Ok(camel) if !camel.messages.is_empty() || !camel.folders.is_empty() => {
            let status = format_camel_status(&camel);
            return loaded_camel_page(camel, status, message_offset, append);
        }
        Ok(_) => {
            return load_local_mail(
                Some("Keine aktiven EDS-Mailkonten; lokaler Evolution-Speicher"),
                append,
            );
        }
        Err(error) => {
            if append {
                return MailCommand::Loaded {
                    messages: Vec::new(),
                    folders: Vec::new(),
                    status: format!("Weitere Nachrichten konnten nicht geladen werden: {error}"),
                    append: true,
                    can_load_more: false,
                };
            }
            return load_local_mail(
                Some(&format!(
                    "EDS/Camel nicht verfügbar ({error}); lokaler Evolution-Speicher"
                )),
                false,
            );
        }
    }
}

fn loaded_camel_page(
    camel: camel_mail_service::CamelMailLoad,
    status: String,
    message_offset: usize,
    append: bool,
) -> MailCommand {
    let can_load_more = camel_has_more_messages(&camel, message_offset);
    MailCommand::Loaded {
        messages: summaries(camel.messages),
        folders: camel.folders,
        status,
        append,
        can_load_more,
    }
}

fn camel_has_more_messages(
    camel: &camel_mail_service::CamelMailLoad,
    message_offset: usize,
) -> bool {
    let mut loaded_per_folder = HashMap::<(&str, &str), usize>::new();
    for message in &camel.messages {
        let Some(location) = message.backend_location.as_ref() else {
            continue;
        };
        *loaded_per_folder
            .entry((location.source_uid.as_str(), location.folder_name.as_str()))
            .or_default() += 1;
    }

    camel.folders.iter().any(|folder| {
        let loaded = loaded_per_folder
            .get(&(folder.source_uid.as_str(), folder.folder_name.as_str()))
            .copied()
            .unwrap_or_default();
        folder.total_count as usize > message_offset.saturating_add(loaded)
    })
}

fn load_local_mail(reason: Option<&str>, append: bool) -> MailCommand {
    let messages = MailStoreService::new().load_local_records();
    let count = messages.len();
    MailCommand::Loaded {
        messages: summaries(messages),
        folders: Vec::new(),
        status: format!(
            "{} · {count} lokale Nachricht(en) geladen",
            reason.unwrap_or("Lokaler Evolution-Speicher")
        ),
        append,
        can_load_more: false,
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
            folder_name: mail.folder_name,
            backend_location: mail.backend_location,
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
            is_batch_selected: false,
            attachments: mail.attachments,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(total_count: u32) -> camel_mail_service::CamelFolderSummary {
        camel_mail_service::CamelFolderSummary {
            source_uid: "account-a".to_string(),
            folder_name: "INBOX".to_string(),
            folder_id: "inbox".to_string(),
            display_name: "Posteingang".to_string(),
            unread_count: 0,
            total_count,
            is_selectable: true,
        }
    }

    #[test]
    fn camel_page_reports_remaining_messages_by_folder() {
        let page = camel_mail_service::CamelMailLoad {
            messages: Vec::new(),
            folders: vec![folder(101)],
            configured_sources: 1,
            warning: None,
        };

        assert!(camel_has_more_messages(&page, 0));
        assert!(camel_has_more_messages(&page, 100));
        assert!(!camel_has_more_messages(&page, 101));
    }

    #[test]
    fn camel_page_counts_loaded_records_for_remaining_check() {
        let mut message = crate::services::mail_store_service::parse_rfc822(
            "eds:account-a:INBOX:uid-101",
            "inbox",
            "From: sender@example.org\nSubject: Page\n\nBody\n",
            true,
        )
        .expect("message should parse");
        message.backend_location = Some(MailBackendLocation {
            source_uid: "account-a".to_string(),
            folder_name: "INBOX".to_string(),
            message_uid: "uid-101".to_string(),
        });

        let page = camel_mail_service::CamelMailLoad {
            messages: vec![message],
            folders: vec![folder(101)],
            configured_sources: 1,
            warning: None,
        };

        assert!(!camel_has_more_messages(&page, 100));
    }

    #[test]
    fn batch_state_actions_change_only_requested_flags() {
        let mark_read = MailBatchAction::SetState {
            is_unread: Some(false),
            is_flagged: None,
        };
        assert_eq!(mark_read.next_state(true, true), Some((false, true)));

        let mark = MailBatchAction::SetState {
            is_unread: None,
            is_flagged: Some(true),
        };
        assert_eq!(mark.next_state(true, false), Some((true, true)));

        assert_eq!(MailBatchAction::Archive.next_state(true, true), None);
    }
}
