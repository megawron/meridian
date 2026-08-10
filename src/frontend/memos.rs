// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 memos component backed by local VJOURNAL records until the ECal
// journal stream and mutation adapter are connected.

use crate::services::calendar_store_service::{CalendarStoreService, MemoRecord};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

#[derive(Debug, Clone)]
pub struct MemoSummary {
    title: String,
    updated: String,
    description: String,
}

#[derive(Debug)]
struct MemoRow {
    memo: MemoSummary,
}

#[derive(Debug)]
enum MemoRowInput {}

#[relm4::factory]
impl FactoryComponent for MemoRow {
    type Init = MemoSummary;
    type Input = MemoRowInput;
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
                set_label: &self.memo.title,
                set_halign: gtk::Align::Start,
            },
            gtk::Label {
                #[watch]
                set_label: &format!("VJOURNAL · {}", self.memo.updated),
                set_halign: gtk::Align::Start,
                add_css_class: "dim-label",
            },
            gtk::Label {
                #[watch]
                set_label: &self.memo.description,
                set_halign: gtk::Align::Start,
                set_wrap: true,
            },
        }
    }

    fn init_model(memo: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { memo }
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

#[derive(Debug)]
pub(crate) enum MemosInput {
    Refresh,
}

#[derive(Debug)]
pub enum MemosCommand {
    Loaded(Vec<MemoSummary>),
}

#[derive(Debug)]
pub(crate) struct MemosModel {
    status: String,
    loading: bool,
    rows: FactoryVecDeque<MemoRow>,
}

#[relm4::component(pub)]
impl Component for MemosModel {
    type Init = ();
    type Input = MemosInput;
    type Output = ();
    type CommandOutput = MemosCommand;

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
                    set_label: "Notizen",
                    add_css_class: "title-2",
                    set_hexpand: true,
                    set_halign: gtk::Align::Start,
                },
                gtk::Button {
                    set_label: "Aktualisieren",
                    connect_clicked => MemosInput::Refresh,
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
            .forward(sender.input_sender(), |_output| MemosInput::Refresh);
        let model = MemosModel {
            status: "Notizen werden geladen…".to_string(),
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

        sender.spawn_oneshot_command(|| load_memos());
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            MemosInput::Refresh => {
                if !self.loading {
                    self.loading = true;
                    self.status = "Notizen werden geladen…".to_string();
                    sender.spawn_oneshot_command(|| load_memos());
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
            MemosCommand::Loaded(memos) => {
                let count = memos.len();
                let mut rows = self.rows.guard();
                rows.clear();
                for memo in memos {
                    rows.push_back(memo);
                }
                self.status = format!("{count} lokale VJOURNAL(s); ECal-Mutationen folgen");
            }
        }
    }
}

fn load_memos() -> MemosCommand {
    let memos = CalendarStoreService::new()
        .load_snapshot()
        .memos
        .into_iter()
        .map(memo_summary)
        .collect();
    MemosCommand::Loaded(memos)
}

fn memo_summary(memo: MemoRecord) -> MemoSummary {
    MemoSummary {
        title: memo.summary,
        updated: memo.updated,
        description: memo.description,
    }
}
