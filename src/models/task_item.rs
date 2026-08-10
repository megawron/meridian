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
    pub fn new(id: &str, title: &str, due_date: &str, priority: &str, is_completed: bool) -> Self {
        let obj: Self = Object::builder().build();
        let imp = obj.imp();
        *imp.id.borrow_mut() = id.to_string();
        *imp.title.borrow_mut() = title.to_string();
        *imp.due_date.borrow_mut() = due_date.to_string();
        *imp.priority.borrow_mut() = priority.to_string();
        *imp.is_completed.borrow_mut() = is_completed;
        obj
    }

    pub fn id(&self) -> String {
        self.imp().id.borrow().clone()
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
}
