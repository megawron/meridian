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

use crate::i18n::gettext;
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
        const NAME: &'static str = "MeridianFilterEditorDialog";
        type Type = super::FilterEditorDialog;
        type ParentType = adw::Dialog;
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
    impl AdwDialogImpl for FilterEditorDialog {}
}

glib::wrapper! {
    pub struct FilterEditorDialog(ObjectSubclass<imp::FilterEditorDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
        FilterAction::AssignColor(_) => 14,
        FilterAction::UnsetColor => 15,
        FilterAction::AssignScore(_) => 16,
        FilterAction::AdjustScore(_) => 17,
        FilterAction::Beep => 18,
        FilterAction::PlaySound(_) => 19,
        FilterAction::RunProgram(_) => 20,
        FilterAction::PipeToProgram(_) => 21,
        FilterAction::ForwardTo(_) => 22,
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
        return Err(gettext("This condition requires a search value"));
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
        0 if target.is_empty() => Err(gettext("A destination folder is required for moving")),
        0 => Ok(FilterAction::MoveToFolder(target)),
        1 if target.is_empty() => Err(gettext("A destination folder is required for copying")),
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
        12 if target.is_empty() => Err(gettext("A label name is required")),
        12 => Ok(FilterAction::SetLabel(target)),
        13 if target.is_empty() => Err(gettext("A label name is required")),
        13 => Ok(FilterAction::ClearLabel(target)),
        14 if target.is_empty() => {
            Err(gettext("A color value is required (e.g. #ff0000 or red)"))
        }
        14 => Ok(FilterAction::AssignColor(target)),
        15 => Ok(FilterAction::UnsetColor),
        16 => target
            .parse::<i32>()
            .map(FilterAction::AssignScore)
            .map_err(|_| gettext("A valid integer is required for scoring")),
        17 => target
            .parse::<i32>()
            .map(FilterAction::AdjustScore)
            .map_err(|_| gettext("An integer (+/-) is required for score adjustment")),
        18 => Ok(FilterAction::Beep),
        19 if target.is_empty() => Err(gettext("A file path is required for the sound")),
        19 => Ok(FilterAction::PlaySound(target)),
        20 if target.is_empty() => Err(gettext("A command is required for the program")),
        20 => Ok(FilterAction::RunProgram(target)),
        21 if target.is_empty() => Err(gettext("A command is required for the pipe")),
        21 => Ok(FilterAction::PipeToProgram(target)),
        22 if target.is_empty() => Err(gettext("An email address is required for forwarding")),
        22 => Ok(FilterAction::ForwardTo(target)),
        _ => Err(gettext("Unknown filter action")),
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
    let field_labels = [
        gettext("Subject"),
        gettext("Sender"),
        gettext("Recipient"),
        gettext("Message Body"),
        gettext("Header"),
        gettext("Label"),
        gettext("Unread"),
        gettext("Flagged / Starred"),
        gettext("Importance"),
        gettext("Attachment"),
        gettext("Date"),
        gettext("Folder"),
        gettext("Junk / Spam"),
    ];
    let field_slices: Vec<&str> = field_labels.iter().map(|s| s.as_str()).collect();
    let field_model = gtk::StringList::new(&field_slices);
    let field = gtk::DropDown::new(Some(field_model), None::<&gtk::Expression>);
    field.set_selected(filter_field_index(condition.field));
    field.set_width_request(135);
    let operator_labels = [
        gettext("contains"),
        gettext("does not contain"),
        gettext("is"),
        gettext("is not"),
        gettext("starts with"),
        gettext("ends with"),
        gettext("before"),
        gettext("after"),
        gettext("exists"),
    ];
    let operator_slices: Vec<&str> = operator_labels.iter().map(|s| s.as_str()).collect();
    let operator_model = gtk::StringList::new(&operator_slices);
    let operator = gtk::DropDown::new(Some(operator_model), None::<&gtk::Expression>);
    operator.set_selected(filter_operator_index(condition.operator));
    operator.set_width_request(125);
    let initial_placeholder = if condition.field == FilterField::Header {
        gettext("Header: Value")
    } else {
        gettext("Value…")
    };
    let value = gtk::Entry::builder()
        .placeholder_text(&initial_placeholder)
        .hexpand(true)
        .build();
    value.set_text(&condition.value);
    let value_for_field = value.clone();
    field.connect_selected_notify(move |dropdown| {
        let placeholder = match dropdown.selected() {
            4 => gettext("Header: Value"),
            _ => gettext("Value…"),
        };
        value_for_field.set_placeholder_text(Some(&placeholder));
    });
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove condition"))
        .css_classes(["flat", "error"])
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
    let action_labels = [
        gettext("Move to Folder"),
        gettext("Copy to Folder"),
        gettext("Mark as Read"),
        gettext("Mark as Unread"),
        gettext("Set Flagged / Starred"),
        gettext("Clear Flagged / Starred"),
        gettext("Set Important"),
        gettext("Clear Important"),
        gettext("Delete"),
        gettext("Stop Processing"),
        gettext("Mark as Junk"),
        gettext("Mark as Not Junk"),
        gettext("Set Label"),
        gettext("Clear Label"),
        gettext("Assign Color"),
        gettext("Unset Color"),
        gettext("Assign Score"),
        gettext("Adjust Score"),
        gettext("Beep"),
        gettext("Play Sound"),
        gettext("Run Program"),
        gettext("Pipe to Program"),
        gettext("Forward To"),
    ];
    let action_slices: Vec<&str> = action_labels.iter().map(|s| s.as_str()).collect();
    let action_model = gtk::StringList::new(&action_slices);
    let action_type = gtk::DropDown::new(Some(action_model), None::<&gtk::Expression>);
    action_type.set_selected(filter_action_index(&action));
    action_type.set_width_request(240);
    let target = gtk::Entry::builder()
        .placeholder_text(gettext("Destination folder, label, or parameter"))
        .hexpand(true)
        .build();
    if let Some(target_val) = action.target_value() {
        target.set_text(target_val);
    }
    target.set_sensitive(action.target_value().is_some());
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(gettext("Remove action"))
        .css_classes(["flat", "error"])
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
        let sel = dropdown.selected();
        let needs_target = matches!(sel, 0 | 1 | 12 | 13 | 14 | 16 | 17 | 19 | 20 | 21 | 22);
        target_for_type.set_sensitive(needs_target);
        if needs_target && target_for_type.text().trim().is_empty() {
            let default_val = match sel {
                0 | 1 => "archive",
                12 | 13 => "work",
                14 => "#ff0000",
                16 => "10",
                17 => "5",
                19 => "/usr/share/sounds/freedesktop/stereo/complete.oga",
                20 => "/usr/bin/notify-send",
                21 => "cat",
                22 => "user@example.com",
                _ => "",
            };
            target_for_type.set_text(default_val);
        }
        let placeholder = match sel {
            0 | 1 => gettext("Destination folder (e.g. archive)"),
            12 | 13 => gettext("Label name (e.g. Important)"),
            14 => gettext("Color code or name (e.g. #ff0000)"),
            16 => gettext("Score (integer, e.g. 10)"),
            17 => gettext("Score delta (e.g. +5 or -10)"),
            19 => gettext("Sound file path (e.g. .wav, .oga)"),
            20 => gettext("Command / script path"),
            21 => gettext("Command for STDIN pipe"),
            22 => gettext("Recipient email address"),
            _ => String::new(),
        };
        target_for_type.set_placeholder_text(Some(&placeholder));
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
            .property("title", &gettext("New Filter Rule"))
            .property("content-width", 640)
            .property("content-height", 560)
            .build()
    }

    pub fn new_for_edit(rule: &MailFilterRule) -> Self {
        let encoded = serde_json::to_string(rule).expect("filter rule should serialize");
        glib::Object::builder()
            .property("title", &gettext("Edit Filter Rule"))
            .property("content-width", 640)
            .property("content-height", 560)
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

        let header_bar = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let btn_cancel = gtk::Button::builder()
            .label(gettext("Cancel"))
            .build();
        let dialog_for_cancel = self.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header_bar.pack_start(&btn_cancel);

        let btn_save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action"])
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
            .title(gettext("Rule Name & Scope"))
            .build();

        let row_rulename = adw::EntryRow::builder()
            .title(gettext("Rule Name"))
            .build();
        group_general.add(&row_rulename);

        let scope_options = [
            gettext("Incoming Mail"),
            gettext("Outgoing Mail"),
            gettext("Manual Only"),
        ];
        let scope_slices: Vec<&str> = scope_options.iter().map(|s| s.as_str()).collect();
        let row_scope = adw::ComboRow::builder()
            .title(gettext("Apply When"))
            .model(&gtk::StringList::new(&scope_slices))
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

        // Conditions. Use dynamic GTK rows so imported Evolution rules with
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
            .label(gettext("Conditions (If…)"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let add_condition = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(gettext("Add condition"))
            .css_classes(["flat", "circular"])
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

        let match_mode_options = [
            gettext("All conditions match (AND)"),
            gettext("Any condition matches (OR)"),
        ];
        let match_mode_slices: Vec<&str> = match_mode_options.iter().map(|s| s.as_str()).collect();
        let group_cond = adw::PreferencesGroup::new();
        let row_match_mode = adw::ComboRow::builder()
            .title(gettext("Condition Logic"))
            .model(&gtk::StringList::new(&match_mode_slices))
            .build();
        row_match_mode.add_prefix(&gtk::Image::from_icon_name("edit-select-all-symbolic"));
        if let Some(rule) = edit_rule.as_ref() {
            row_match_mode.set_selected(if rule.match_all { 0 } else { 1 });
        }
        group_cond.add(&row_match_mode);

        content_box.append(&condition_section);
        content_box.append(&group_cond);

        // Actions. Folder targets are free-form so custom Evolution folder
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
            .label(gettext("Actions (Then…)"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let add_action = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(gettext("Add action"))
            .css_classes(["flat", "circular"])
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
                    dialog.show_toast(&gettext("Please enter a rule name"));
                    return;
                }
                let conditions = match condition_editors
                    .borrow()
                    .iter()
                    .enumerate()
                    .map(|(index, editor)| {
                        condition_from_editor(editor)
                            .map_err(|error| format!("{}: {error}", format!("{} {}", gettext("Condition"), index + 1)))
                    })
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(conditions) if !conditions.is_empty() => conditions,
                    Ok(_) => {
                        dialog.show_toast(&gettext("Please add at least one filter condition"));
                        return;
                    }
                    Err(error) => {
                        dialog.show_toast(&error);
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
                        dialog.show_toast(&gettext("Please add at least one filter action"));
                        return;
                    }
                    Err(error) => {
                        dialog.show_toast(&error);
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
                        dialog.show_toast(&gettext("Filter rule saved"));
                        dialog.close();
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist mail filter rule");
                        dialog.show_toast(&format!(
                            "{}: {error}",
                            gettext("Could not save filter rule")
                        ));
                    }
                }
            }
        ));

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_child(Some(&toolbar_view));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
        }
    }
}

impl Default for FilterEditorDialog {
    fn default() -> Self {
        Self::new()
    }
}
