// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 calendar component. EDS source discovery and local iCalendar import
// are reported through one typed command result.

use crate::services::calendar_store_service::{
    CalendarEventRecord, CalendarSnapshot, CalendarStoreService,
};
use crate::services::eds_service;
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

#[derive(Debug, Clone)]
struct EventSummary {
    title: String,
    start: String,
    location: String,
}

#[derive(Debug)]
struct EventRow {
    event: EventSummary,
}

#[derive(Debug)]
enum EventRowInput {}

#[relm4::factory]
impl FactoryComponent for EventRow {
    type Init = EventSummary;
    type Input = EventRowInput;
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
                set_label: &self.event.title,
                set_halign: gtk::Align::Start,
            },
            gtk::Label {
                #[watch]
                set_label: &format!(
                    "{}{}",
                    self.event.start,
                    if self.event.location.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", self.event.location)
                    }
                ),
                set_halign: gtk::Align::Start,
                add_css_class: "dim-label",
            },
        }
    }

    fn init_model(event: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { event }
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

#[derive(Debug)]
pub(crate) enum CalendarInput {
    Refresh,
}

#[derive(Debug)]
pub enum CalendarCommand {
    Loaded {
        snapshot: CalendarSnapshot,
        source_status: String,
    },
}

#[derive(Debug)]
pub(crate) struct CalendarModel {
    status: String,
    loading: bool,
    rows: FactoryVecDeque<EventRow>,
}

#[relm4::component(pub)]
impl Component for CalendarModel {
    type Init = ();
    type Input = CalendarInput;
    type Output = ();
    type CommandOutput = CalendarCommand;

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
                    set_label: "Kalender",
                    add_css_class: "title-2",
                    set_hexpand: true,
                    set_halign: gtk::Align::Start,
                },
                gtk::Button {
                    set_label: "Aktualisieren",
                    connect_clicked => CalendarInput::Refresh,
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
            .forward(sender.input_sender(), |_output| CalendarInput::Refresh);
        let model = CalendarModel {
            status: "Kalender werden geladen…".to_string(),
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

        sender.oneshot_command(async { load_calendar().await });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            CalendarInput::Refresh => {
                if !self.loading {
                    self.loading = true;
                    self.status = "Kalender werden geladen…".to_string();
                    sender.oneshot_command(async { load_calendar().await });
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
            CalendarCommand::Loaded {
                snapshot,
                source_status,
            } => {
                let events = snapshot
                    .events
                    .iter()
                    .cloned()
                    .map(event_summary)
                    .collect::<Vec<_>>();
                let event_count = events.len();
                let mut rows = self.rows.guard();
                rows.clear();
                for event in events {
                    rows.push_back(event);
                }
                self.status = format!(
                    "{source_status} · {event_count} lokale VEVENT(s), {} Aufgabe(n), {} Memo(s)",
                    snapshot.tasks.len(),
                    snapshot.memos.len()
                );
            }
        }
    }
}

fn event_summary(event: CalendarEventRecord) -> EventSummary {
    EventSummary {
        title: event.summary,
        start: if event.start.is_empty() {
            "Ohne Startzeit".to_string()
        } else {
            event.start
        },
        location: event.location,
    }
}

async fn load_calendar() -> CalendarCommand {
    let source_status = eds_service::discover_sources(eds_service::EdsSourceKind::Calendar)
        .await
        .status_text();
    let snapshot = CalendarStoreService::new().load_snapshot();
    CalendarCommand::Loaded {
        snapshot,
        source_status,
    }
}
