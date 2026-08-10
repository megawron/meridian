// SPDX-License-Identifier: GPL-3.0-or-later
//
// SearchFolderObject — GObject model representing Virtual Search Folders (vFolders).
//
// Allows combining emails across multiple accounts based on dynamic search criteria:
// - "Ungelesen (Alle Konten)"
// - "Wichtig & Markiert"
// - "Letzte 7 Tage"

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct SearchFolderObject {
        pub id: RefCell<String>,
        pub name: RefCell<String>,
        pub icon_name: RefCell<String>,
        pub query_expression: RefCell<String>,
        pub unread_count: RefCell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchFolderObject {
        const NAME: &'static str = "EvolutionSearchFolderObject";
        type Type = super::SearchFolderObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for SearchFolderObject {}
}

glib::wrapper! {
    pub struct SearchFolderObject(ObjectSubclass<imp::SearchFolderObject>);
}

impl SearchFolderObject {
    pub fn new(id: &str, name: &str, icon_name: &str, query: &str, unread_count: u32) -> Self {
        let obj: Self = Object::builder().build();
        let imp = obj.imp();
        *imp.id.borrow_mut() = id.to_string();
        *imp.name.borrow_mut() = name.to_string();
        *imp.icon_name.borrow_mut() = icon_name.to_string();
        *imp.query_expression.borrow_mut() = query.to_string();
        *imp.unread_count.borrow_mut() = unread_count;
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

    pub fn query_expression(&self) -> String {
        self.imp().query_expression.borrow().clone()
    }

    pub fn unread_count(&self) -> u32 {
        *self.imp().unread_count.borrow()
    }
}
