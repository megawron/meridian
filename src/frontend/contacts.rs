// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 contacts component. EDS address-book discovery and local vCard
// imports are represented by one explicit command result.

use crate::services::contact_store_service::ContactStoreService;
use crate::services::eds_service;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

#[derive(Debug, Clone)]
pub(crate) struct ContactSummary {
    name: String,
    email: String,
    title: String,
}

#[derive(Debug)]
struct ContactRow {
    contact: ContactSummary,
}

#[derive(Debug)]
enum ContactRowInput {}

#[relm4::factory]
impl FactoryComponent for ContactRow {
    type Init = ContactSummary;
    type Input = ContactRowInput;
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 2,
            set_margin_start: 12,
            set_margin_end: 12,
            set_margin_top: 8,
            set_margin_bottom: 8,

            gtk::Label {
                #[watch]
                set_label: &self.contact.name,
                set_halign: gtk::Align::Start,
            },
            gtk::Label {
                #[watch]
                set_label: &format!(
                    "{}{}",
                    self.contact.email,
                    if self.contact.title.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", self.contact.title)
                    }
                ),
                set_halign: gtk::Align::Start,
                add_css_class: "dim-label",
            },
        }
    }

    fn init_model(
        contact: Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        Self { contact }
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

#[derive(Debug)]
pub(crate) enum ContactsInput {
    Refresh,
}

#[derive(Debug)]
pub enum ContactsCommand {
    Loaded {
        contacts: Vec<ContactSummary>,
        source_status: String,
    },
}

#[derive(Debug)]
pub(crate) struct ContactsModel {
    status: String,
    loading: bool,
    rows: FactoryVecDeque<ContactRow>,
}

#[relm4::component(pub)]
impl Component for ContactsModel {
    type Init = ();
    type Input = ContactsInput;
    type Output = ();
    type CommandOutput = ContactsCommand;

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

                gtk::Label {
                    set_label: "Kontakte",
                    add_css_class: "title-2",
                    set_hexpand: true,
                    set_halign: gtk::Align::Start,
                },
                gtk::Button {
                    set_label: "Aktualisieren",
                    connect_clicked => ContactsInput::Refresh,
                },
            },

            gtk::Label {
                #[watch]
                set_label: &model.status,
                set_halign: gtk::Align::Start,
                add_css_class: "dim-label",
                set_margin_start: 12,
                set_margin_end: 12,
                set_margin_bottom: 8,
            },

            #[name(content_box)]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_vexpand: true,
                set_hexpand: true,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .forward(sender.input_sender(), |_output| ContactsInput::Refresh);
        let model = ContactsModel {
            status: "Adressbücher werden geladen…".to_string(),
            loading: false,
            rows,
        };
        let row_box = model.rows.widget();
        let widgets = view_output!();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(row_box)
            .build();
        widgets.content_box.append(&scroll);

        sender.oneshot_command(async { load_contacts().await });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            ContactsInput::Refresh => {
                if !self.loading {
                    self.loading = true;
                    self.status = "Adressbücher werden geladen…".to_string();
                    sender.oneshot_command(async { load_contacts().await });
                }
            }
        }
    }

    fn update_cmd(
        &mut self,
        message: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        self.loading = false;
        match message {
            ContactsCommand::Loaded {
                contacts,
                source_status,
            } => {
                let count = contacts.len();
                let mut rows = self.rows.guard();
                rows.clear();
                for contact in contacts {
                    rows.push_back(contact);
                }
                self.status = format!("{source_status} · {count} lokale(r) Kontakt(e)");
            }
        }
    }
}

async fn load_contacts() -> ContactsCommand {
    let source_status = eds_service::discover_sources(eds_service::EdsSourceKind::AddressBook)
        .await
        .status_text();
    let contacts = ContactStoreService::new()
        .load_contacts()
        .into_iter()
        .map(|record| ContactSummary {
            name: record.contact.fn_name,
            email: record.contact.email,
            title: record.contact.title,
        })
        .collect();
    ContactsCommand::Loaded {
        contacts,
        source_status,
    }
}
