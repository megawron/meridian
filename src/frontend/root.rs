// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 application shell. The EDS/Camel/ECal/EBook services are injected
// through framework-neutral services; this component owns only UI state.

use super::calendar::{CalendarInput, CalendarModel};
use super::compose::ComposeModel;
use super::contacts::{ContactsInput, ContactsModel};
use super::mail::{MailInput, MailModel};
use super::memos::{MemosInput, MemosModel};
use super::tasks::{TasksInput, TasksModel};
use relm4::adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

pub fn launch_preview(app: &adw::Application) {
    let mut controller = FrontendModel::builder().launch(());
    let window = controller.widget().clone();
    window.set_application(Some(app));
    window.present();
    controller.detach_runtime();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendView {
    Mail,
    Calendar,
    Contacts,
    Tasks,
    Memos,
}

impl FrontendView {
    fn stack_name(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Contacts => "contacts",
            Self::Tasks => "tasks",
            Self::Memos => "memos",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::Calendar => "Kalender",
            Self::Contacts => "Kontakte",
            Self::Tasks => "Aufgaben",
            Self::Memos => "Notizen",
        }
    }
}

#[derive(Debug)]
pub enum FrontendInput {
    Navigate(FrontendView),
    RefreshMail,
    Compose,
}

#[derive(Debug)]
struct FrontendModel {
    active_view: FrontendView,
    compose: Controller<ComposeModel>,
    mail: Controller<MailModel>,
    calendar: Controller<CalendarModel>,
    contacts: Controller<ContactsModel>,
    tasks: Controller<TasksModel>,
    memos: Controller<MemosModel>,
}

#[relm4::component]
impl Component for FrontendModel {
    type Init = ();
    type Input = FrontendInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("Evolution"),
            set_default_width: 1400,
            set_default_height: 900,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 6,
                    set_margin_start: 12,
                    set_margin_end: 12,
                    set_margin_top: 10,
                    set_margin_bottom: 10,

                    gtk::Button {
                        set_label: "Mail",
                        connect_clicked => FrontendInput::Navigate(FrontendView::Mail),
                    },
                    gtk::Button {
                        set_label: "Kalender",
                        connect_clicked => FrontendInput::Navigate(FrontendView::Calendar),
                    },
                    gtk::Button {
                        set_label: "Kontakte",
                        connect_clicked => FrontendInput::Navigate(FrontendView::Contacts),
                    },
                    gtk::Button {
                        set_label: "Aufgaben",
                        connect_clicked => FrontendInput::Navigate(FrontendView::Tasks),
                    },
                    gtk::Button {
                        set_label: "Notizen",
                        connect_clicked => FrontendInput::Navigate(FrontendView::Memos),
                    },
                    gtk::Button {
                        set_label: "Neue Nachricht",
                        set_icon_name: "mail-message-new-symbolic",
                        connect_clicked => FrontendInput::Compose,
                    },
                    gtk::Button {
                        set_label: "Mail aktualisieren",
                        set_hexpand: true,
                        set_halign: gtk::Align::End,
                        connect_clicked => FrontendInput::RefreshMail,
                    },
                },

                #[name(page_stack)]
                gtk::Stack {
                    #[watch]
                    set_visible_child_name: model.active_view.stack_name(),
                    set_vexpand: true,
                    set_hexpand: true,
                },
            }
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let compose = ComposeModel::builder().launch(()).detach();
        let mail = MailModel::builder().launch(()).detach();
        let calendar = CalendarModel::builder().launch(()).detach();
        let contacts = ContactsModel::builder().launch(()).detach();
        let tasks = TasksModel::builder().launch(()).detach();
        let memos = MemosModel::builder().launch(()).detach();
        let model = FrontendModel {
            active_view: FrontendView::Mail,
            compose,
            mail,
            calendar,
            contacts,
            tasks,
            memos,
        };
        let mail_widget = model.mail.widget().clone();
        let calendar_widget = model.calendar.widget().clone();
        let contacts_widget = model.contacts.widget().clone();
        let tasks_widget = model.tasks.widget().clone();
        let memos_widget = model.memos.widget().clone();
        let widgets = view_output!();

        widgets.page_stack.add_titled(
            &mail_widget,
            Some(FrontendView::Mail.stack_name()),
            FrontendView::Mail.title(),
        );
        widgets.page_stack.add_titled(
            &calendar_widget,
            Some(FrontendView::Calendar.stack_name()),
            FrontendView::Calendar.title(),
        );
        widgets.page_stack.add_titled(
            &contacts_widget,
            Some(FrontendView::Contacts.stack_name()),
            FrontendView::Contacts.title(),
        );
        widgets.page_stack.add_titled(
            &tasks_widget,
            Some(FrontendView::Tasks.stack_name()),
            FrontendView::Tasks.title(),
        );
        widgets.page_stack.add_titled(
            &memos_widget,
            Some(FrontendView::Memos.stack_name()),
            FrontendView::Memos.title(),
        );

        let _ = sender;
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            FrontendInput::Navigate(view) => {
                self.active_view = view;
            }
            FrontendInput::RefreshMail => {
                let _ = self.mail.sender().send(MailInput::Refresh);
                let _ = self.calendar.sender().send(CalendarInput::Refresh);
                let _ = self.contacts.sender().send(ContactsInput::Refresh);
                let _ = self.tasks.sender().send(TasksInput::Refresh);
                let _ = self.memos.sender().send(MemosInput::Refresh);
            }
            FrontendInput::Compose => {
                self.compose.widget().set_transient_for(Some(root));
                self.compose.widget().present();
            }
        }
    }
}
