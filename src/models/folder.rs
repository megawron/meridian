// SPDX-License-Identifier: GPL-3.0-or-later
//
// FolderObject — GObject model representing a mail folder for GListModel/GtkListView.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct FolderObject {
        pub id: RefCell<String>,
        pub name: RefCell<String>,
        pub icon_name: RefCell<String>,
        pub unread_count: RefCell<u32>,
        pub total_count: RefCell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderObject {
        const NAME: &'static str = "EvolutionFolderObject";
        type Type = super::FolderObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for FolderObject {}
}

glib::wrapper! {
    pub struct FolderObject(ObjectSubclass<imp::FolderObject>);
}

impl FolderObject {
    pub fn new(id: &str, name: &str, icon_name: &str, unread_count: u32, total_count: u32) -> Self {
        let obj: Self = Object::builder().build();
        let imp = obj.imp();
        *imp.id.borrow_mut() = id.to_string();
        *imp.name.borrow_mut() = name.to_string();
        *imp.icon_name.borrow_mut() = icon_name.to_string();
        *imp.unread_count.borrow_mut() = unread_count;
        *imp.total_count.borrow_mut() = total_count;
        obj
    }

    pub fn id(&self) -> String {
        self.imp().id.borrow().clone()
    }

    pub fn name(&self) -> String {
        self.imp().name.borrow().clone()
    }

    pub fn icon_name(&self) -> String {
        self.imp().icon_name.borrow().clone()
    }

    pub fn unread_count(&self) -> u32 {
        *self.imp().unread_count.borrow()
    }

    pub fn total_count(&self) -> u32 {
        *self.imp().total_count.borrow()
    }
}
