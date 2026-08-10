// SPDX-License-Identifier: GPL-3.0-or-later
//
// TaskItemObject — GObject model representing a task (VTODO) for GTK4 ListModels.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct TaskItemObject {
        pub id: RefCell<String>,
        pub source_uid: RefCell<String>,
        pub component: RefCell<String>,
        pub title: RefCell<String>,
        pub due_date: RefCell<String>,
        pub priority: RefCell<String>,
        pub is_completed: RefCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TaskItemObject {
        const NAME: &'static str = "EvolutionTaskItemObject";
        type Type = super::TaskItemObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for TaskItemObject {}
}

glib::wrapper! {
    pub struct TaskItemObject(ObjectSubclass<imp::TaskItemObject>);
}

impl TaskItemObject {
    pub fn new(
        source_uid: &str,
        id: &str,
        title: &str,
        due_date: &str,
        priority: &str,
        is_completed: bool,
        component: &str,
    ) -> Self {
        let obj: Self = Object::builder().build();
        let imp = obj.imp();
        *imp.id.borrow_mut() = id.to_string();
        *imp.source_uid.borrow_mut() = source_uid.to_string();
        *imp.component.borrow_mut() = component.to_string();
        *imp.title.borrow_mut() = title.to_string();
        *imp.due_date.borrow_mut() = due_date.to_string();
        *imp.priority.borrow_mut() = priority.to_string();
        *imp.is_completed.borrow_mut() = is_completed;
        obj
    }

    pub fn id(&self) -> String {
        self.imp().id.borrow().clone()
    }

    pub fn source_uid(&self) -> String {
        self.imp().source_uid.borrow().clone()
    }

    pub fn component(&self) -> String {
        self.imp().component.borrow().clone()
    }

    pub fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }

    pub fn due_date(&self) -> String {
        self.imp().due_date.borrow().clone()
    }

    pub fn priority(&self) -> String {
        self.imp().priority.borrow().clone()
    }

    pub fn is_completed(&self) -> bool {
        *self.imp().is_completed.borrow()
    }

    pub fn set_completed(&self, completed: bool) {
        *self.imp().is_completed.borrow_mut() = completed;
    }

    pub fn set_component(&self, component: String) {
        *self.imp().component.borrow_mut() = component;
    }
}
