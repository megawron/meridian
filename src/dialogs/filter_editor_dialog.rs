// SPDX-License-Identifier: GPL-3.0-or-later
//
// FilterEditorDialog — Dialog for creating and managing mail rules and filters.
//
// Features:
// - Rule name & Scope (Incoming / Outgoing / Manual)
// - Matching Conditions (field + contains/equality/prefix/existence operator)
// - Action Types (Move to folder, Copy, Mark as read, Set flag, Delete, Play sound)

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::mail_filter_service::{
    FilterAction, FilterCondition, FilterField, FilterOperator, FilterScope, MailFilterRule,
    MailFilterStore,
};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct FilterEditorDialog {
        pub edit_rule_json: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FilterEditorDialog {
        const NAME: &'static str = "EvolutionFilterEditorDialog";
        type Type = super::FilterEditorDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for FilterEditorDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES
                .get_or_init(|| vec![glib::ParamSpecString::builder("edit-rule-json").build()])
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "edit-rule-json" {
                *self.edit_rule_json.borrow_mut() = value.get().expect("string property");
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-rule-json" => self.edit_rule_json.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

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

struct ConditionEditor {
    id: u64,
    field: gtk::DropDown,
    operator: gtk::DropDown,
    value: gtk::Entry,
    row: gtk::Box,
}

struct ActionEditor {
    id: u64,
    action_type: gtk::DropDown,
    target: gtk::Entry,
    row: gtk::Box,
}

fn filter_field_index(field: FilterField) -> u32 {
    match field {
        FilterField::Subject => 0,
        FilterField::Sender => 1,
        FilterField::Recipient => 2,
        FilterField::Body => 3,
        FilterField::Header => 4,
        FilterField::Label => 5,
        FilterField::Unread => 6,
        FilterField::Flagged => 7,
        FilterField::Important => 8,
        FilterField::Attachment => 9,
        FilterField::Date => 10,
        FilterField::Folder => 11,
        FilterField::Junk => 12,
    }
}

fn filter_operator_index(operator: FilterOperator) -> u32 {
    match operator {
        FilterOperator::Contains => 0,
        FilterOperator::NotContains => 1,
        FilterOperator::Equals => 2,
        FilterOperator::NotEquals => 3,
        FilterOperator::StartsWith => 4,
        FilterOperator::EndsWith => 5,
        FilterOperator::Before => 6,
        FilterOperator::After => 7,
        FilterOperator::Exists => 8,
    }
}

fn filter_operator_from_index(index: u32) -> FilterOperator {
    match index {
        1 => FilterOperator::NotContains,
        2 => FilterOperator::Equals,
        3 => FilterOperator::NotEquals,
        4 => FilterOperator::StartsWith,
        5 => FilterOperator::EndsWith,
        6 => FilterOperator::Before,
        7 => FilterOperator::After,
        8 => FilterOperator::Exists,
        _ => FilterOperator::Contains,
    }
}

fn filter_action_index(action: &FilterAction) -> u32 {
    match action {
        FilterAction::MoveToFolder(_) => 0,
        FilterAction::CopyToFolder(_) => 1,
        FilterAction::MarkRead => 2,
        FilterAction::MarkUnread => 3,
        FilterAction::SetFlagged => 4,
        FilterAction::ClearFlagged => 5,
        FilterAction::SetImportant => 6,
        FilterAction::ClearImportant => 7,
        FilterAction::Delete => 8,
        FilterAction::StopProcessing => 9,
        FilterAction::SetJunk => 10,
        FilterAction::ClearJunk => 11,
        FilterAction::SetLabel(_) => 12,
        FilterAction::ClearLabel(_) => 13,
    }
}

fn condition_from_editor(editor: &ConditionEditor) -> Result<FilterCondition, String> {
    let value = editor.value.text().trim().to_string();
    let field = match editor.field.selected() {
        1 => FilterField::Sender,
        2 => FilterField::Recipient,
        3 => FilterField::Body,
        4 => FilterField::Header,
        5 => FilterField::Label,
        6 => FilterField::Unread,
        7 => FilterField::Flagged,
        8 => FilterField::Important,
        9 => FilterField::Attachment,
        10 => FilterField::Date,
        11 => FilterField::Folder,
        12 => FilterField::Junk,
        _ => FilterField::Subject,
    };
    let operator = filter_operator_from_index(editor.operator.selected());
    if value.is_empty() && !operator.allows_empty_value(field) {
        return Err("Diese Bedingung benötigt einen Suchwert".to_string());
    }
    Ok(FilterCondition {
        field,
        value,
        operator,
    })
}

fn action_from_editor(editor: &ActionEditor) -> Result<FilterAction, String> {
    let target = editor.target.text().trim().to_string();
    match editor.action_type.selected() {
        0 if target.is_empty() => Err("Für das Verschieben ist ein Zielordner erforderlich".into()),
        0 => Ok(FilterAction::MoveToFolder(target)),
        1 if target.is_empty() => Err("Für das Kopieren ist ein Zielordner erforderlich".into()),
        1 => Ok(FilterAction::CopyToFolder(target)),
        2 => Ok(FilterAction::MarkRead),
        3 => Ok(FilterAction::MarkUnread),
        4 => Ok(FilterAction::SetFlagged),
        5 => Ok(FilterAction::ClearFlagged),
        6 => Ok(FilterAction::SetImportant),
        7 => Ok(FilterAction::ClearImportant),
        8 => Ok(FilterAction::Delete),
        9 => Ok(FilterAction::StopProcessing),
        10 => Ok(FilterAction::SetJunk),
        11 => Ok(FilterAction::ClearJunk),
        12 if target.is_empty() => Err("Für das Label ist ein Name erforderlich".into()),
        12 => Ok(FilterAction::SetLabel(target)),
        13 if target.is_empty() => Err("Für das Label ist ein Name erforderlich".into()),
        _ => Ok(FilterAction::ClearLabel(target)),
    }
}

fn append_condition_editor(
    container: &gtk::Box,
    editors: &Rc<RefCell<Vec<ConditionEditor>>>,
    next_id: &Rc<Cell<u64>>,
    condition: FilterCondition,
) {
    let id = next_id.get();
    next_id.set(id.saturating_add(1));
    let field_model = gtk::StringList::new(&[
        "Betreff",
        "Absender",
        "Empfänger",
        "Nachrichteninhalt",
        "Headerfeld",
        "Label",
        "Ungelesen",
        "Stern-/Markierungsstatus",
        "Wichtigkeit",
        "Anhang vorhanden",
        "Datum",
        "Ordner",
        "Spam/Junk",
    ]);
    let field = gtk::DropDown::new(Some(field_model), None::<&gtk::Expression>);
    field.set_selected(filter_field_index(condition.field));
    field.set_width_request(150);
    let operator_model = gtk::StringList::new(&[
        "enthält",
        "enthält nicht",
        "ist",
        "ist nicht",
        "beginnt mit",
        "endet mit",
        "vor",
        "nach",
        "existiert",
    ]);
    let operator = gtk::DropDown::new(Some(operator_model), None::<&gtk::Expression>);
    operator.set_selected(filter_operator_index(condition.operator));
    operator.set_width_request(135);
    let value = gtk::Entry::builder()
        .placeholder_text("Suchwert oder Headername")
        .hexpand(true)
        .build();
    value.set_text(&condition.value);
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Bedingung entfernen")
        .css_classes(vec!["flat", "error"])
        .valign(gtk::Align::Center)
        .build();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    row.append(&field);
    row.append(&operator);
    row.append(&value);
    row.append(&remove);
    container.append(&row);

    editors.borrow_mut().push(ConditionEditor {
        id,
        field,
        operator,
        value,
        row: row.clone(),
    });

    let editors_for_remove = Rc::clone(editors);
    let container_for_remove = container.clone();
    remove.connect_clicked(move |_| {
        let removed = {
            let mut editors = editors_for_remove.borrow_mut();
            if editors.len() > 1 {
                editors
                    .iter()
                    .position(|editor| editor.id == id)
                    .map(|index| editors.remove(index))
            } else {
                if let Some(editor) = editors.iter().find(|editor| editor.id == id) {
                    editor.value.set_text("");
                }
                None
            }
        };
        if let Some(editor) = removed {
            container_for_remove.remove(&editor.row);
        }
    });
}

fn append_action_editor(
    container: &gtk::Box,
    editors: &Rc<RefCell<Vec<ActionEditor>>>,
    next_id: &Rc<Cell<u64>>,
    action: FilterAction,
) {
    let id = next_id.get();
    next_id.set(id.saturating_add(1));
    let action_model = gtk::StringList::new(&[
        "In Ordner verschieben",
        "In Ordner kopieren",
        "Als gelesen markieren",
        "Als ungelesen markieren",
        "Stern-/Markierungsstatus setzen",
        "Stern-/Markierungsstatus entfernen",
        "Als wichtig markieren",
        "Wichtigkeitsmarkierung entfernen",
        "Löschen",
        "Weitere Filterregeln nicht ausführen",
        "Als Spam markieren",
        "Als kein Spam markieren",
        "Label setzen",
        "Label entfernen",
    ]);
    let action_type = gtk::DropDown::new(Some(action_model), None::<&gtk::Expression>);
    action_type.set_selected(filter_action_index(&action));
    action_type.set_width_request(220);
    let target = gtk::Entry::builder()
        .placeholder_text("Zielordner oder Label")
        .hexpand(true)
        .build();
    match &action {
        FilterAction::MoveToFolder(folder)
        | FilterAction::CopyToFolder(folder)
        | FilterAction::SetLabel(folder)
        | FilterAction::ClearLabel(folder) => target.set_text(folder),
        _ => {}
    }
    target.set_sensitive(matches!(
        action,
        FilterAction::MoveToFolder(_)
            | FilterAction::CopyToFolder(_)
            | FilterAction::SetLabel(_)
            | FilterAction::ClearLabel(_)
    ));
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Aktion entfernen")
        .css_classes(vec!["flat", "error"])
        .valign(gtk::Align::Center)
        .build();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    row.append(&action_type);
    row.append(&target);
    row.append(&remove);
    container.append(&row);

    let target_for_type = target.clone();
    action_type.connect_selected_notify(move |dropdown| {
        let needs_target = matches!(dropdown.selected(), 0 | 1 | 12 | 13);
        target_for_type.set_sensitive(needs_target);
        if needs_target && target_for_type.text().trim().is_empty() {
            target_for_type.set_text(if matches!(dropdown.selected(), 12 | 13) {
                "work"
            } else {
                "archive"
            });
        }
    });

    editors.borrow_mut().push(ActionEditor {
        id,
        action_type,
        target,
        row: row.clone(),
    });

    let editors_for_remove = Rc::clone(editors);
    let container_for_remove = container.clone();
    remove.connect_clicked(move |_| {
        let removed = {
            let mut editors = editors_for_remove.borrow_mut();
            if editors.len() > 1 {
                editors
                    .iter()
                    .position(|editor| editor.id == id)
                    .map(|index| editors.remove(index))
            } else {
                None
            }
        };
        if let Some(editor) = removed {
            container_for_remove.remove(&editor.row);
        }
    });
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

    pub fn new_for_edit(rule: &MailFilterRule) -> Self {
        let encoded = serde_json::to_string(rule).expect("filter rule should serialize");
        glib::Object::builder()
            .property("title", "Filterregel bearbeiten")
            .property("modal", true)
            .property("default-width", 620)
            .property("default-height", 540)
            .property("edit-rule-json", encoded)
            .build()
    }

    fn build_ui(&self) {
        let edit_rule = self
            .imp()
            .edit_rule_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<MailFilterRule>(raw).ok());
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
        if let Some(rule) = edit_rule.as_ref() {
            row_scope.set_selected(match rule.scope {
                FilterScope::Incoming => 0,
                FilterScope::Outgoing => 1,
                FilterScope::Manual => 2,
            });
        }
        group_general.add(&row_scope);

        content_box.append(&group_general);

        // Conditions.  Use dynamic GTK rows so imported Evolution rules with
        // multiple conditions can also be created and edited here.
        let condition_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        let condition_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let condition_title = gtk::Label::builder()
            .label("Bedingungen (Wenn…)")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["heading"])
            .build();
        let add_condition = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Weitere Bedingung hinzufügen")
            .css_classes(vec!["flat", "circular"])
            .build();
        condition_header.append(&condition_title);
        condition_header.append(&add_condition);
        condition_section.append(&condition_header);
        let condition_rows_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        condition_section.append(&condition_rows_box);

        let condition_editors = Rc::new(RefCell::new(Vec::<ConditionEditor>::new()));
        let next_condition_id = Rc::new(Cell::new(0));
        let initial_conditions = edit_rule
            .as_ref()
            .map(|rule| {
                rule.effective_conditions()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|conditions| !conditions.is_empty())
            .unwrap_or_else(|| {
                vec![FilterCondition {
                    field: FilterField::Subject,
                    value: String::new(),
                    operator: FilterOperator::Contains,
                }]
            });
        for condition in initial_conditions {
            append_condition_editor(
                &condition_rows_box,
                &condition_editors,
                &next_condition_id,
                condition,
            );
        }
        add_condition.connect_clicked(glib::clone!(
            #[strong]
            condition_rows_box,
            #[strong]
            condition_editors,
            #[strong]
            next_condition_id,
            move |_| {
                append_condition_editor(
                    &condition_rows_box,
                    &condition_editors,
                    &next_condition_id,
                    FilterCondition {
                        field: FilterField::Subject,
                        value: String::new(),
                        operator: FilterOperator::Contains,
                    },
                );
            }
        ));

        let group_cond = adw::PreferencesGroup::builder()
            .description("Alle Bedingungen müssen zutreffen oder mindestens eine (ODER).")
            .build();
        let row_match_mode = adw::ComboRow::builder()
            .title("Bedingungsverknüpfung")
            .model(&gtk::StringList::new(&[
                "Alle Bedingungen (UND)",
                "Eine Bedingung (ODER)",
            ]))
            .build();
        if let Some(rule) = edit_rule.as_ref() {
            row_match_mode.set_selected(if rule.match_all { 0 } else { 1 });
        }
        group_cond.add(&row_match_mode);

        content_box.append(&condition_section);
        content_box.append(&group_cond);

        // Actions.  Folder targets are free-form so custom Evolution folder
        // names survive editing instead of being forced into demo choices.
        let action_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .build();
        let action_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let action_title = gtk::Label::builder()
            .label("Aktionen (Dann…)")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["heading"])
            .build();
        let add_action = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Weitere Aktion hinzufügen")
            .css_classes(vec!["flat", "circular"])
            .build();
        action_header.append(&action_title);
        action_header.append(&add_action);
        action_section.append(&action_header);
        let action_rows_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        action_section.append(&action_rows_box);

        let action_editors = Rc::new(RefCell::new(Vec::<ActionEditor>::new()));
        let next_action_id = Rc::new(Cell::new(0));
        let initial_actions = edit_rule
            .as_ref()
            .map(|rule| {
                rule.effective_actions()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|actions| !actions.is_empty())
            .unwrap_or_else(|| vec![FilterAction::MoveToFolder("archive".to_string())]);
        for action in initial_actions {
            append_action_editor(&action_rows_box, &action_editors, &next_action_id, action);
        }
        add_action.connect_clicked(glib::clone!(
            #[strong]
            action_rows_box,
            #[strong]
            action_editors,
            #[strong]
            next_action_id,
            move |_| {
                append_action_editor(
                    &action_rows_box,
                    &action_editors,
                    &next_action_id,
                    FilterAction::MarkRead,
                );
            }
        ));
        content_box.append(&action_section);

        let edit_id = edit_rule.as_ref().map(|rule| rule.id.clone());
        let edit_enabled = edit_rule.as_ref().map(|rule| rule.enabled).unwrap_or(true);
        if let Some(rule) = edit_rule.as_ref() {
            row_rulename.set_text(&rule.name);
        }

        btn_save.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            row_rulename,
            #[strong]
            row_scope,
            #[strong]
            row_match_mode,
            #[strong]
            condition_editors,
            #[strong]
            action_editors,
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
                let conditions = match condition_editors
                    .borrow()
                    .iter()
                    .enumerate()
                    .map(|(index, editor)| {
                        condition_from_editor(editor)
                            .map_err(|error| format!("Bedingung {}: {error}", index + 1))
                    })
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(conditions) if !conditions.is_empty() => conditions,
                    Ok(_) => {
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(
                                    "Bitte mindestens eine Filterbedingung anlegen",
                                ));
                            }
                        }
                        return;
                    }
                    Err(error) => {
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(&error));
                            }
                        }
                        return;
                    }
                };

                let actions = match action_editors
                    .borrow()
                    .iter()
                    .map(action_from_editor)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(actions) if !actions.is_empty() => actions,
                    Ok(_) => {
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(
                                    "Bitte mindestens eine Filteraktion anlegen",
                                ));
                            }
                        }
                        return;
                    }
                    Err(error) => {
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(&error));
                            }
                        }
                        return;
                    }
                };

                let scope = match row_scope.selected() {
                    1 => FilterScope::Outgoing,
                    2 => FilterScope::Manual,
                    _ => FilterScope::Incoming,
                };
                let condition = conditions[0].clone();
                let action = actions[0].clone();
                let rule = MailFilterRule {
                    id: edit_id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name,
                    enabled: edit_enabled,
                    scope,
                    condition: condition.clone(),
                    action: action.clone(),
                    conditions,
                    actions,
                    match_all: row_match_mode.selected() == 0,
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
