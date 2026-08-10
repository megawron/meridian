// SPDX-License-Identifier: GPL-3.0-or-later
//
// FilterEditorDialog — Dialog for creating and managing mail rules and filters.
//
// Features:
// - Rule name & Scope (Incoming / Outgoing / Manual)
// - Matching Conditions (Subject, Sender, Recipient, Body, Header)
// - Action Types (Move to folder, Copy, Mark as read, Set flag, Delete, Play sound)

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::services::mail_filter_service::{
    FilterAction, FilterCondition, FilterField, FilterScope, MailFilterRule, MailFilterStore,
};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct FilterEditorDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for FilterEditorDialog {
        const NAME: &'static str = "EvolutionFilterEditorDialog";
        type Type = super::FilterEditorDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for FilterEditorDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for FilterEditorDialog {}
    impl WindowImpl for FilterEditorDialog {}
    impl AdwWindowImpl for FilterEditorDialog {}
}

glib::wrapper! {
    pub struct FilterEditorDialog(ObjectSubclass<imp::FilterEditorDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl FilterEditorDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Filterregel erstellen")
            .property("modal", true)
            .property("default-width", 620)
            .property("default-height", 540)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        // Rule Name
        let group_general = adw::PreferencesGroup::builder()
            .title("Regelname & Ausführung")
            .build();

        let row_rulename = adw::EntryRow::builder().title("Regelname").build();
        group_general.add(&row_rulename);

        let row_scope = adw::ComboRow::builder()
            .title("Anwenden bei")
            .model(&gtk::StringList::new(&[
                "Posteingang (Automatisch bei Eingang)",
                "Postausgang (Beim Senden)",
                "Manuelle Ausführung",
            ]))
            .build();
        group_general.add(&row_scope);

        content_box.append(&group_general);

        // Conditions
        let group_cond = adw::PreferencesGroup::builder()
            .title("Bedingungen (Wenn…)")
            .description("Alle Bedingungen müssen zutreffen (UND-Verknüpfung)")
            .build();

        let row_cond_type = adw::ComboRow::builder()
            .title("Feld")
            .model(&gtk::StringList::new(&[
                "Betreff enthält",
                "Absender enthält",
                "Nachrichteninhalt enthält",
                "Headerfeld existiert",
            ]))
            .build();
        group_cond.add(&row_cond_type);

        let row_cond_value = adw::EntryRow::builder().title("Suchwert").build();
        group_cond.add(&row_cond_value);

        content_box.append(&group_cond);

        // Actions
        let group_actions = adw::PreferencesGroup::builder()
            .title("Aktionen (Dann…)")
            .build();

        let row_action_type = adw::ComboRow::builder()
            .title("Aktion")
            .model(&gtk::StringList::new(&[
                "In Ordner verschieben",
                "In Ordner kopieren",
                "Als gelesen markieren",
                "Wichtig-Flag setzen",
                "Löschen",
            ]))
            .build();
        group_actions.add(&row_action_type);

        let row_target_folder = adw::ComboRow::builder()
            .title("Zielordner")
            .model(&gtk::StringList::new(&[
                "Finanzen / Rechnungen",
                "Archiv",
                "Arbeit",
                "Papierkorb",
            ]))
            .build();
        group_actions.add(&row_target_folder);

        content_box.append(&group_actions);

        btn_save.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            row_rulename,
            #[strong]
            row_scope,
            #[strong]
            row_cond_type,
            #[strong]
            row_cond_value,
            #[strong]
            row_action_type,
            #[strong]
            row_target_folder,
            move |_| {
                let name = row_rulename.text().trim().to_string();
                if name.is_empty() {
                    tracing::warn!("Cannot save a mail filter without a name");
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window.add_toast(&adw::Toast::new("Bitte einen Regelnamen eingeben"));
                        }
                    }
                    return;
                }

                let scope = match row_scope.selected() {
                    1 => FilterScope::Outgoing,
                    2 => FilterScope::Manual,
                    _ => FilterScope::Incoming,
                };
                let field = match row_cond_type.selected() {
                    1 => FilterField::Sender,
                    2 => FilterField::Body,
                    3 => FilterField::Header,
                    _ => FilterField::Subject,
                };
                let target_folder = match row_target_folder.selected() {
                    0 => "finances",
                    1 => "archive",
                    2 => "work",
                    3 => "trash",
                    _ => "archive",
                };
                let action = match row_action_type.selected() {
                    1 => FilterAction::CopyToFolder(target_folder.to_string()),
                    2 => FilterAction::MarkRead,
                    3 => FilterAction::SetFlagged,
                    4 => FilterAction::Delete,
                    _ => FilterAction::MoveToFolder(target_folder.to_string()),
                };
                let rule = MailFilterRule {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    enabled: true,
                    scope,
                    condition: FilterCondition {
                        field,
                        value: row_cond_value.text().to_string(),
                    },
                    action,
                };

                match MailFilterStore::default().upsert(rule) {
                    Ok(()) => {
                        tracing::info!("Mail filter rule persisted");
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new("Filterregel gespeichert"));
                            }
                        }
                        dialog.close();
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist mail filter rule");
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(&format!(
                                    "Filterregel konnte nicht gespeichert werden: {error}"
                                )));
                            }
                        }
                    }
                }
            }
        ));

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for FilterEditorDialog {
    fn default() -> Self {
        Self::new()
    }
}
