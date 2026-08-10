// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 tasks component backed by the local iCalendar importer until the
// ECal VTODO stream and mutation adapter are connected.

use crate::services::calendar_store_service::{CalendarStoreService, TaskRecord};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender, FactoryVecDeque};
use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

#[derive(Debug, Clone)]
pub struct TaskSummary {
    title: String,
    due: String,
    priority: u8,
    completed: bool,
}

#[derive(Debug)]
struct TaskRow {
    task: TaskSummary,
}

#[derive(Debug)]
enum TaskRowInput {}

#[relm4::factory]
impl FactoryComponent for TaskRow {
    type Init = TaskSummary;
    type Input = TaskRowInput;
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 10,
            set_margin_start: 12,
            set_margin_end: 12,
            set_margin_top: 8,
            set_margin_bottom: 8,

            gtk::CheckButton {
                set_active: self.task.completed,
                set_sensitive: false,
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 2,

                gtk::Label {
                    #[watch]
                    set_label: &self.task.title,
                    set_halign: gtk::Align::Start,
                },
                gtk::Label {
                    #[watch]
                    set_label: &format!(
                        "Fällig: {} · Priorität: {}",
                        if self.task.due.is_empty() {
                            "ohne Datum"
                        } else {
                            &self.task.due
                        },
                        if self.task.priority == 0 {
                            "normal".to_string()
                        } else {
                            self.task.priority.to_string()
                        }
                    ),
                    set_halign: gtk::Align::Start,
                    add_css_class: "dim-label",
                },
            },
        }
    }

    fn init_model(task: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { task }
    }

    fn update(&mut self, _message: Self::Input, _sender: FactorySender<Self>) {}
}

#[derive(Debug)]
pub(crate) enum TasksInput {
    Refresh,
}

#[derive(Debug)]
pub enum TasksCommand {
    Loaded(Vec<TaskSummary>),
}

#[derive(Debug)]
pub(crate) struct TasksModel {
    status: String,
    loading: bool,
    rows: FactoryVecDeque<TaskRow>,
}

#[relm4::component(pub)]
impl Component for TasksModel {
    type Init = ();
    type Input = TasksInput;
    type Output = ();
    type CommandOutput = TasksCommand;

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
                    set_label: "Aufgaben",
                    add_css_class: "title-2",
                    set_hexpand: true,
                    set_halign: gtk::Align::Start,
                },
                gtk::Button {
                    set_label: "Aktualisieren",
                    connect_clicked => TasksInput::Refresh,
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
            .forward(sender.input_sender(), |_output| TasksInput::Refresh);
        let model = TasksModel {
            status: "Aufgaben werden geladen…".to_string(),
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

        sender.spawn_oneshot_command(|| load_tasks());
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            TasksInput::Refresh => {
                if !self.loading {
                    self.loading = true;
                    self.status = "Aufgaben werden geladen…".to_string();
                    sender.spawn_oneshot_command(|| load_tasks());
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
            TasksCommand::Loaded(tasks) => {
                let count = tasks.len();
                let mut rows = self.rows.guard();
                rows.clear();
                for task in tasks {
                    rows.push_back(task);
                }
                self.status = format!("{count} lokale VTODO(s); ECal-Mutationen folgen");
            }
        }
    }
}

fn load_tasks() -> TasksCommand {
    let tasks = CalendarStoreService::new()
        .load_snapshot()
        .tasks
        .into_iter()
        .map(task_summary)
        .collect();
    TasksCommand::Loaded(tasks)
}

fn task_summary(task: TaskRecord) -> TaskSummary {
    TaskSummary {
        title: task.summary,
        due: task.due,
        priority: task.priority,
        completed: task.completed,
    }
}
