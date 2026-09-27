// SPDX-License-Identifier: GPL-3.0-or-later
//
// DraftsDialog — portable local draft management.
//
// This is deliberately separate from the native EDS/Camel Drafts folder.  It
// gives the GTK4-rs frontend a complete local fallback on systems where the
// Linux EDS development/runtime stack is not available yet.  Once Camel is
// active, the native folder remains authoritative and this dialog is still
// useful for recovering drafts created by the portable fallback.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

use crate::services::draft_store_service::{DraftMessage, DraftStoreService};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct DraftsDialog {
        pub list: RefCell<Option<gtk::ListBox>>,
        pub status: RefCell<Option<gtk::Label>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DraftsDialog {
        const NAME: &'static str = "EvolutionDraftsDialog";
        type Type = super::DraftsDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for DraftsDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for DraftsDialog {}
    impl AdwDialogImpl for DraftsDialog {}
}

glib::wrapper! {
    pub struct DraftsDialog(ObjectSubclass<imp::DraftsDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DraftsDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Lokale Entwürfe")
            .property("content-width", 720)
            .property("content-height", 560)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Entwürfe aktualisieren")
            .css_classes(vec!["flat"])
            .build();
        header_bar.pack_start(&refresh_button);

        let new_button = gtk::Button::builder()
            .label("Neue E-Mail")
            .icon_name("mail-message-new-symbolic")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&new_button);
        toolbar_view.add_top_bar(&header_bar);

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(8)
            .css_classes(vec!["dim-label"])
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(16)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&status);
        content.append(&scrolled);
        toolbar_view.set_content(Some(&content));
        self.set_child(Some(&toolbar_view));

        self.imp().list.replace(Some(list));
        self.imp().status.replace(Some(status));

        refresh_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.reload()
        ));
        new_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.present_new_composer()
        ));

        self.reload();
    }

    fn reload(&self) {
        let Some(list) = self.imp().list.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let mut drafts = DraftStoreService::default().load();
        drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

        if drafts.is_empty() {
            self.set_status(
                "Keine lokalen Entwürfe vorhanden. Entwürfe werden automatisch gespeichert, \
                 sobald der Composer Inhalt enthält.",
            );
            return;
        }

        self.set_status(&format!(
            "{} lokale{} Entwurf{} — synchronisierte Entwürfe werden auf Linux separat verwaltet.",
            drafts.len(),
            if drafts.len() == 1 { "n" } else { "e" },
            if drafts.len() == 1 { "" } else { "e" },
        ));
        for draft in drafts {
            self.append_draft_row(&list, draft);
        }
    }

    fn append_draft_row(&self, list: &gtk::ListBox, draft: DraftMessage) {
        let title = if draft.subject.trim().is_empty() {
            "(Ohne Betreff)".to_string()
        } else {
            draft.subject.clone()
        };
        let subtitle = draft_summary(&draft);
        let row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(&subtitle)
            .activatable(true)
            .build();

        let open_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Entwurf öffnen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Entwurf löschen")
            .css_classes(vec!["flat", "destructive-action"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&open_button);
        row.add_suffix(&delete_button);
        list.append(&row);

        let draft_for_open = draft.clone();
        let dialog_weak = self.downgrade();
        open_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.present_draft(&draft_for_open);
            }
        });

        let draft_id = draft.id.clone();
        let dialog_weak = self.downgrade();
        delete_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.delete_draft(&draft_id);
            }
        });

        let draft_for_open = draft;
        let dialog_weak = self.downgrade();
        row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.present_draft(&draft_for_open);
            }
        });
    }

    fn present_new_composer(&self) {
        self.present_composer(crate::dialogs::ComposeDialog::new());
    }

    fn present_draft(&self, draft: &DraftMessage) {
        self.present_composer(crate::dialogs::ComposeDialog::from_draft(draft));
    }

    fn present_composer(&self, composer: crate::dialogs::ComposeDialog) {
        if let Some(parent) = self.root().and_downcast::<gtk::Window>() {
            composer.set_transient_for(Some(&parent));
        }
        composer.present();
    }

    fn delete_draft(&self, id: &str) {
        match DraftStoreService::default().delete(id) {
            Ok(true) => {
                self.show_parent_toast("Entwurf gelöscht");
                self.reload();
            }
            Ok(false) => {
                self.reload();
            }
            Err(error) => {
                self.show_parent_toast(&format!("Entwurf konnte nicht gelöscht werden: {error}"))
            }
        }
    }

    fn set_status(&self, message: &str) {
        if let Some(status) = self.imp().status.borrow().as_ref() {
            status.set_text(message);
        }
    }

    fn show_parent_toast(&self, message: &str) {
        if let Some(window) = self.root().and_downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
        tracing::warn!(%message, "Draft manager result");
    }
}

impl Default for DraftsDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn draft_summary(draft: &DraftMessage) -> String {
    let mut parts = Vec::new();
    if let Some(method) = draft.calendar_method.as_deref() {
        parts.push(format!("iTIP-Antwort: {method}"));
    }
    if !draft.to.trim().is_empty() {
        parts.push(format!("An: {}", draft.to.trim()));
    }
    if !draft.cc.trim().is_empty() {
        parts.push(format!("Cc: {}", draft.cc.trim()));
    }
    if !draft.updated_at.trim().is_empty() {
        parts.push(format!("geändert: {}", draft.updated_at.trim()));
    }
    if !draft.attachments.is_empty() {
        parts.push(format!(
            "{} Anhang{}",
            draft.attachments.len(),
            if draft.attachments.len() == 1 {
                ""
            } else {
                "änge"
            }
        ));
    }
    if parts.is_empty() {
        "Leerer lokaler Entwurf".to_string()
    } else {
        parts.join(" · ")
    }
}
