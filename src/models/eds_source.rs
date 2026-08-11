// SPDX-License-Identifier: GPL-3.0-or-later
//
// EdsSourceObject — GTK model for a mail source discovered through EDS/GOA.

use glib::Object;
use gtk::glib;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct EdsSourceObject {
        pub uid: RefCell<String>,
        pub display_name: RefCell<String>,
        pub backend_name: RefCell<String>,
        pub enabled: RefCell<bool>,
        pub origin: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EdsSourceObject {
        const NAME: &'static str = "EvolutionEdsSourceObject";
        type Type = super::EdsSourceObject;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for EdsSourceObject {}
}

glib::wrapper! {
    pub struct EdsSourceObject(ObjectSubclass<imp::EdsSourceObject>);
}

impl EdsSourceObject {
    pub fn new(uid: &str, display_name: &str, backend_name: &str, enabled: bool) -> Self {
        let object: Self = Object::builder().build();
        let imp = object.imp();
        *imp.uid.borrow_mut() = uid.to_string();
        *imp.display_name.borrow_mut() = display_name.to_string();
        *imp.backend_name.borrow_mut() = backend_name.to_string();
        *imp.enabled.borrow_mut() = enabled;
        *imp.origin.borrow_mut() = "EDS/GOA".to_string();
        object
    }

    pub fn from_source(source: &crate::services::eds_service::EdsSource) -> Self {
        Self::from_source_with_origin(source, "EDS/GOA")
    }

    pub fn from_source_with_origin(
        source: &crate::services::eds_service::EdsSource,
        origin: &str,
    ) -> Self {
        let object = Self::new(
            &source.uid,
            &source.display_name,
            &source.backend_name,
            source.enabled,
        );
        *object.imp().origin.borrow_mut() = origin.to_string();
        object
    }

    pub fn from_account_profile(
        profile: &crate::services::account_config_service::AccountProfile,
    ) -> Self {
        let object = Self::new(
            &profile.id,
            &profile.display_name,
            profile.protocol.as_str(),
            false,
        );
        *object.imp().origin.borrow_mut() = "lokales Profil".to_string();
        object
    }

    pub fn uid(&self) -> String {
        self.imp().uid.borrow().clone()
    }

    pub fn display_name(&self) -> String {
        self.imp().display_name.borrow().clone()
    }

    pub fn backend_name(&self) -> String {
        self.imp().backend_name.borrow().clone()
    }

    pub fn enabled(&self) -> bool {
        *self.imp().enabled.borrow()
    }

    pub fn origin(&self) -> String {
        self.imp().origin.borrow().clone()
    }
}
