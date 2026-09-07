// SPDX-License-Identifier: GPL-3.0-or-later
//
// FolderObject — GObject model representing a mail folder for GListModel/GtkListView.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    use std::sync::LazyLock;

    #[derive(Debug, Default)]
    pub struct FolderObject {
        pub id: RefCell<String>,
        pub name: RefCell<String>,
        pub icon_name: RefCell<String>,
        pub unread_count: RefCell<u32>,
        pub total_count: RefCell<u32>,
        pub source_uid: RefCell<Option<String>>,
        pub folder_name: RefCell<Option<String>>,
        pub parent_id: RefCell<Option<String>>,
        pub selectable: RefCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderObject {
        const NAME: &'static str = "EvolutionFolderObject";
        type Type = super::FolderObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for FolderObject {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: LazyLock<Vec<glib::subclass::Signal>> = LazyLock::new(|| {
                vec![glib::subclass::Signal::builder("counts-changed").build()]
            });
            SIGNALS.as_ref()
        }
    }
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

    pub fn new_backend(
        id: &str,
        name: &str,
        icon_name: &str,
        unread_count: u32,
        total_count: u32,
        source_uid: &str,
        folder_name: &str,
        parent_id: Option<&str>,
        selectable: bool,
    ) -> Self {
        let obj = Self::new(id, name, icon_name, unread_count, total_count);
        let imp = obj.imp();
        *imp.source_uid.borrow_mut() = Some(source_uid.to_string());
        *imp.folder_name.borrow_mut() = Some(folder_name.to_string());
        *imp.parent_id.borrow_mut() = parent_id.map(str::to_string);
        *imp.selectable.borrow_mut() = selectable;
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

    pub fn set_counts(&self, unread_count: u32, total_count: u32) {
        let changed = {
            let mut unread = self.imp().unread_count.borrow_mut();
            let mut total = self.imp().total_count.borrow_mut();
            if *unread != unread_count || *total != total_count {
                *unread = unread_count;
                *total = total_count;
                true
            } else {
                false
            }
        };
        if changed {
            use glib::prelude::ObjectExt;
            self.emit_by_name::<()>("counts-changed", &[]);
        }
    }

    pub fn source_uid(&self) -> Option<String> {
        self.imp().source_uid.borrow().clone()
    }

    pub fn folder_name(&self) -> Option<String> {
        self.imp().folder_name.borrow().clone()
    }

    pub fn parent_id(&self) -> Option<String> {
        self.imp().parent_id.borrow().clone()
    }

    pub fn selectable(&self) -> bool {
        *self.imp().selectable.borrow()
    }
}
