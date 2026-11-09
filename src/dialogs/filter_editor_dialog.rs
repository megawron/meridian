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
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecString::builder("edit-rule-json")
                    .construct()
                    .build()]
            })
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
        FilterField::AllMessages => 13,
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
        FilterOperator::MatchesRegex => 9,
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
        9 => FilterOperator::MatchesRegex,
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
        13 => FilterField::AllMessages,
        _ => FilterField::Subject,
    };
    let operator = if field == FilterField::AllMessages {
        FilterOperator::Exists
    } else {
        filter_operator_from_index(editor.operator.selected())
    };
    if value.is_empty() && !operator.allows_empty_value(field) {
        return Err(gettext("This condition requires a search value"));
    }
    Ok(FilterCondition {
        field,
        value: if field == FilterField::AllMessages {
            String::new()
        } else {
            value
        },
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

fn update_condition_widgets_visibility(
    field: &gtk::DropDown,
    operator: &gtk::DropDown,
    value: &gtk::Entry,
) {
    let field_sel = field.selected();
    let op_sel = operator.selected();
    let is_all_messages = field_sel == 13;
    let is_exists_without_value = op_sel == 8 && field_sel != 4;
    operator.set_visible(!is_all_messages);
    value.set_visible(!is_all_messages && !is_exists_without_value);
    field.set_hexpand(is_all_messages);
    operator.set_hexpand(!is_all_messages && is_exists_without_value);

    let placeholder = if field_sel == 4 {
        gettext("Header: Value")
    } else if op_sel == 9 {
        gettext("Regular expression…")
    } else {
        gettext("Value or * wildcard…")
    };
    value.set_placeholder_text(Some(&placeholder));
}

fn append_condition_editor(
    container: &gtk::Box,
    editors: &Rc<RefCell<Vec<ConditionEditor>>>,
    next_id: &Rc<Cell<u64>>,
    group_cond: &adw::PreferencesGroup,
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
        gettext("All Messages"),
    ];
    let field_slices: Vec<&str> = field_labels.iter().map(|s| s.as_str()).collect();
    let field_model = gtk::StringList::new(&field_slices);
    let field = gtk::DropDown::new(Some(field_model), None::<&gtk::Expression>);
    field.set_selected(filter_field_index(condition.field));
    field.set_width_request(165);
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
        gettext("matches regex"),
    ];
    let operator_slices: Vec<&str> = operator_labels.iter().map(|s| s.as_str()).collect();
    let operator_model = gtk::StringList::new(&operator_slices);
    let operator = gtk::DropDown::new(Some(operator_model), None::<&gtk::Expression>);
    operator.set_selected(filter_operator_index(condition.operator));
    operator.set_width_request(155);
    let value = gtk::Entry::builder()
        .hexpand(true)
        .build();
    value.set_text(&condition.value);
    update_condition_widgets_visibility(&field, &operator, &value);
    {
        let operator_c = operator.clone();
        let value_c = value.clone();
        field.connect_selected_notify(move |dropdown| {
            update_condition_widgets_visibility(dropdown, &operator_c, &value_c);
        });
    }
    {
        let field_c = field.clone();
        let value_c = value.clone();
        operator.connect_selected_notify(move |dropdown| {
            update_condition_widgets_visibility(&field_c, dropdown, &value_c);
        });
    }
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

    {
        let mut list = editors.borrow_mut();
        list.push(ConditionEditor {
            id,
            field,
            operator,
            value,
            row: row.clone(),
        });
        group_cond.set_visible(list.len() > 1);
    }

    let editors_for_remove = Rc::clone(editors);
    let container_for_remove = container.clone();
    let group_cond_for_remove = group_cond.clone();
    remove.connect_clicked(move |_| {
        let (removed, remaining) = {
            let mut editors = editors_for_remove.borrow_mut();
            let removed = if editors.len() > 1 {
                editors
                    .iter()
                    .position(|editor| editor.id == id)
                    .map(|index| editors.remove(index))
            } else {
                if let Some(editor) = editors.iter().find(|editor| editor.id == id) {
                    editor.value.set_text("");
                }
                None
            };
            (removed, editors.len())
        };
        if let Some(editor) = removed {
            container_for_remove.remove(&editor.row);
        }
        group_cond_for_remove.set_visible(remaining > 1);
    });
}

#[derive(Clone, Debug)]
struct FilterFolderTreeItem {
    raw_target: String,
    leaf_label: String,
    full_search_text: String,
    icon_name: &'static str,
    depth: usize,
}

#[derive(Clone, Debug)]
struct FilterFolderAccountGroup {
    account_id: String,
    account_label: String,
    items: Vec<FilterFolderTreeItem>,
}

fn folder_icon_for_name(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower == "inbox" || lower == "posteingang" {
        "mail-inbox-symbolic"
    } else if lower.contains("sent") || lower.contains("gesendet") {
        "mail-send-symbolic"
    } else if lower.contains("draft") || lower.contains("entw") {
        "mail-drafts-symbolic"
    } else if lower.contains("trash") || lower.contains("papierkorb") || lower.contains("deleted") {
        "user-trash-symbolic"
    } else if lower.contains("junk") || lower.contains("spam") {
        "mail-mark-junk-symbolic"
    } else if lower.contains("star") || lower.contains("markiert") || lower.contains("flag") {
        "starred-symbolic"
    } else {
        "folder-symbolic"
    }
}

fn collect_filter_folder_account_groups(current_target: &str) -> Vec<FilterFolderAccountGroup> {
    let mut groups: Vec<FilterFolderAccountGroup> = Vec::new();
    let mut seen_targets = std::collections::HashSet::<String>::new();

    // 1. Collect live folder summaries from active MailPage + SQLite caches on disk
    let mut all_summaries = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|w| w.downcast::<crate::window::MeridianWindow>().ok())
        .and_then(|win| win.mail_page())
        .map(|mp| mp.cached_folder_summaries())
        .unwrap_or_default();

    for cache in crate::services::evolution_summary_cache::discover_meridian_mail_caches() {
        let cache_canon = cache.source_uid.strip_prefix("eds:").unwrap_or(&cache.source_uid);
        let already_loaded = all_summaries.iter().any(|s| {
            let s_canon = s.source_uid.strip_prefix("eds:").unwrap_or(&s.source_uid);
            s_canon.eq_ignore_ascii_case(cache_canon)
        });
        if !already_loaded {
            if let Ok(cached_folders) =
                crate::services::evolution_summary_cache::load_folders_from_sqlite(
                    &cache.folders_db,
                    &cache.source_uid,
                    &cache.display_name,
                )
            {
                all_summaries.extend(cached_folders);
            }
        }
    }

    let profiles = crate::services::account_config_service::AccountConfigStore::default().load();
    let mut covered_sources = std::collections::HashSet::<String>::new();

    for profile in profiles.iter().filter(|p| p.enabled) {
        let acct_label = if !profile.email.trim().is_empty() {
            profile.email.trim().to_string()
        } else {
            profile.display_name.trim().to_string()
        };
        let eds_uid =
            crate::services::account_config_service::eds_source_uid_for_email(&profile.email);
        let prof_canon = profile.id.strip_prefix("eds:").unwrap_or(&profile.id);
        let eds_canon = eds_uid.strip_prefix("eds:").unwrap_or(&eds_uid);

        let matching: Vec<_> = all_summaries
            .iter()
            .filter(|f| {
                let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
                f.is_selectable
                    && !f.folder_name.starts_with('.')
                    && !f.folder_name.contains("/.")
                    && f.folder_name != "[Gmail]"
                    && !f.folder_name.eq_ignore_ascii_case("[Google Mail]")
                    && (f_canon.eq_ignore_ascii_case(prof_canon)
                        || (!eds_canon.is_empty() && f_canon.eq_ignore_ascii_case(eds_canon))
                        || f.source_display_name.eq_ignore_ascii_case(&profile.display_name)
                        || f.source_display_name.eq_ignore_ascii_case(&profile.email))
            })
            .collect();

        let mut items = Vec::new();
        if !matching.is_empty() {
            for f in matching {
                let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
                covered_sources.insert(f_canon.to_ascii_lowercase());
                let raw_target = format!("{f_canon}::{}", f.folder_name);
                if seen_targets.insert(raw_target.to_ascii_lowercase()) {
                    let clean_path = f
                        .folder_name
                        .strip_prefix("[Gmail]/")
                        .or_else(|| f.folder_name.strip_prefix("[Google Mail]/"))
                        .unwrap_or(&f.folder_name);
                    let depth = clean_path.matches('/').count();
                    let leaf_raw = clean_path.rsplit('/').next().unwrap_or(clean_path);
                    let leaf_label = match leaf_raw.to_ascii_lowercase().as_str() {
                        "inbox" => gettext("Inbox"),
                        "sent" | "sent mail" | "sent items" => gettext("Sent"),
                        "drafts" => gettext("Drafts"),
                        "trash" | "deleted" | "deleted items" => gettext("Trash"),
                        "junk" | "spam" => gettext("Junk"),
                        "archive" => gettext("Archive"),
                        _ => f
                            .display_name
                            .strip_prefix("[Gmail]/")
                            .or_else(|| f.display_name.strip_prefix("[Google Mail]/"))
                            .and_then(|s| s.rsplit('/').next())
                            .unwrap_or(leaf_raw)
                            .to_string(),
                    };
                    items.push(FilterFolderTreeItem {
                        full_search_text: format!("{acct_label} / {clean_path} {leaf_label}"),
                        icon_name: folder_icon_for_name(leaf_raw),
                        raw_target,
                        leaf_label,
                        depth,
                    });
                }
            }
        } else {
            let uid = if !eds_canon.is_empty() {
                eds_canon
            } else {
                prof_canon
            };
            covered_sources.insert(uid.to_ascii_lowercase());
            for (raw_fname, localized) in [
                ("INBOX", gettext("Inbox")),
                ("Archive", gettext("Archive")),
                ("Sent", gettext("Sent")),
                ("Drafts", gettext("Drafts")),
                ("Trash", gettext("Trash")),
                ("Junk", gettext("Junk")),
            ] {
                let raw_target = format!("{uid}::{raw_fname}");
                if seen_targets.insert(raw_target.to_ascii_lowercase()) {
                    items.push(FilterFolderTreeItem {
                        full_search_text: format!("{acct_label} / {localized} {raw_fname}"),
                        icon_name: folder_icon_for_name(raw_fname),
                        raw_target,
                        leaf_label: localized,
                        depth: 0,
                    });
                }
            }
        }

        if !items.is_empty() {
            groups.push(FilterFolderAccountGroup {
                account_id: format!("acct:{prof_canon}"),
                account_label: acct_label,
                items,
            });
        }
    }

    // Also group any remaining discovered summaries not in AccountConfigStore
    let mut extra_by_source: std::collections::BTreeMap<String, (String, Vec<FilterFolderTreeItem>)> =
        std::collections::BTreeMap::new();
    for f in &all_summaries {
        let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
        if f_canon.eq_ignore_ascii_case("local")
            || f.source_display_name.to_ascii_lowercase().contains("computer")
            || covered_sources.contains(&f_canon.to_ascii_lowercase())
            || !f.is_selectable
            || f.folder_name.starts_with('.')
            || f.folder_name.contains("/.")
            || f.folder_name == "[Gmail]"
            || f.folder_name.eq_ignore_ascii_case("[Google Mail]")
        {
            continue;
        }
        let raw_target = format!("{f_canon}::{}", f.folder_name);
        if !seen_targets.insert(raw_target.to_ascii_lowercase()) {
            continue;
        }
        let acct_label = if f.source_display_name.trim().is_empty() {
            f_canon.to_string()
        } else {
            f.source_display_name.trim().to_string()
        };
        let clean_path = f
            .folder_name
            .strip_prefix("[Gmail]/")
            .or_else(|| f.folder_name.strip_prefix("[Google Mail]/"))
            .unwrap_or(&f.folder_name);
        let depth = clean_path.matches('/').count();
        let leaf_raw = clean_path.rsplit('/').next().unwrap_or(clean_path);
        let leaf_label = match leaf_raw.to_ascii_lowercase().as_str() {
            "inbox" => gettext("Inbox"),
            "sent" | "sent mail" | "sent items" => gettext("Sent"),
            "drafts" => gettext("Drafts"),
            "trash" | "deleted" | "deleted items" => gettext("Trash"),
            "junk" | "spam" => gettext("Junk"),
            "archive" => gettext("Archive"),
            _ => leaf_raw.to_string(),
        };
        let entry = extra_by_source
            .entry(f_canon.to_string())
            .or_insert_with(|| (acct_label.clone(), Vec::new()));
        entry.1.push(FilterFolderTreeItem {
            full_search_text: format!("{acct_label} / {clean_path} {leaf_label}"),
            icon_name: folder_icon_for_name(leaf_raw),
            raw_target,
            leaf_label,
            depth,
        });
    }
    for (src_uid, (acct_label, items)) in extra_by_source {
        groups.push(FilterFolderAccountGroup {
            account_id: format!("acct:{src_uid}"),
            account_label: acct_label,
            items,
        });
    }

    // 2. "On This Computer" local folders (built-in + full custom subfolder tree)
    let local_prefix = gettext("On This Computer");
    let mut local_items = Vec::new();
    for (raw_id, label, icon) in [
        ("local:inbox", gettext("Inbox"), "mail-inbox-symbolic"),
        ("local:archive", gettext("Archive"), "folder-symbolic"),
        ("local:sent", gettext("Sent"), "mail-send-symbolic"),
        ("local:drafts", gettext("Drafts"), "mail-drafts-symbolic"),
        ("local:trash", gettext("Trash"), "user-trash-symbolic"),
        ("local:junk", gettext("Junk"), "mail-mark-junk-symbolic"),
    ] {
        seen_targets.insert(raw_id.to_string());
        local_items.push(FilterFolderTreeItem {
            raw_target: raw_id.to_string(),
            full_search_text: format!("{local_prefix} / {label}"),
            leaf_label: label,
            icon_name: icon,
            depth: 0,
        });
    }

    let mut custom_paths = std::collections::BTreeSet::<String>::new();
    for custom_id in crate::services::mail_store_service::MailStoreService::new()
        .local_folder_definitions()
    {
        let clean = custom_id.strip_prefix("custom:").unwrap_or(&custom_id);
        let mut prefix = String::new();
        for segment in clean.split('/').map(str::trim).filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            custom_paths.insert(prefix.clone());
        }
    }
    for path in custom_paths {
        let target = format!("local:{path}");
        if seen_targets.insert(target.to_ascii_lowercase()) {
            let depth = path.matches('/').count();
            let leaf = path.rsplit('/').next().unwrap_or(&path).to_string();
            local_items.push(FilterFolderTreeItem {
                raw_target: target,
                full_search_text: format!("{local_prefix} / {path}"),
                leaf_label: leaf,
                icon_name: "folder-symbolic",
                depth,
            });
        }
    }

    let trimmed = current_target.trim();
    if !trimmed.is_empty() && !seen_targets.contains(&trimmed.to_ascii_lowercase()) {
        local_items.insert(
            0,
            FilterFolderTreeItem {
                raw_target: trimmed.to_string(),
                leaf_label: crate::dialogs::preferences_dialog::format_filter_folder_target(trimmed),
                full_search_text: crate::dialogs::preferences_dialog::format_filter_folder_target(trimmed),
                icon_name: "folder-symbolic",
                depth: 0,
            },
        );
    }

    groups.push(FilterFolderAccountGroup {
        account_id: "local".to_string(),
        account_label: local_prefix,
        items: local_items,
    });

    groups
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
    let initial_sel = filter_action_index(&action);
    action_type.set_selected(initial_sel);
    action_type.set_width_request(240);
    let target = gtk::Entry::builder()
        .placeholder_text(gettext("Destination folder, label, or parameter"))
        .hexpand(true)
        .build();
    if let Some(target_val) = action.target_value() {
        target.set_text(target_val);
    } else if matches!(initial_sel, 0 | 1) {
        target.set_text("local:inbox");
    }
    target.set_sensitive(action.target_value().is_some());

    // Searchable folder dropdown for Move to Folder / Copy to Folder
    let folder_btn_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let update_folder_label = {
        let folder_btn_label = folder_btn_label.clone();
        move |raw_target: &str| {
            let trimmed = raw_target.trim();
            if trimmed.is_empty() {
                folder_btn_label.set_label(&gettext("Select folder…"));
            } else {
                folder_btn_label.set_label(
                    &crate::dialogs::preferences_dialog::format_filter_folder_target(trimmed),
                );
            }
        }
    };
    update_folder_label(&target.text());

    let folder_btn_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    folder_btn_content.append(&gtk::Image::from_icon_name("folder-symbolic"));
    folder_btn_content.append(&folder_btn_label);
    folder_btn_content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

    let folder_popover = gtk::Popover::builder()
        .has_arrow(true)
        .build();
    let popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .width_request(320)
        .build();
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search folder…"))
        .hexpand(true)
        .build();
    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(230)
        .max_content_height(290)
        .build();
    let folder_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["navigation-sidebar"])
        .build();
    list_scroll.set_child(Some(&folder_list));

    let new_folder_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let new_folder_entry = gtk::Entry::builder()
        .placeholder_text(gettext("New local folder (e.g. Projects)…"))
        .hexpand(true)
        .build();
    let new_folder_btn = gtk::Button::builder()
        .icon_name("folder-new-symbolic")
        .tooltip_text(gettext("Create Local Folder"))
        .css_classes(["flat"])
        .build();
    new_folder_box.append(&new_folder_entry);
    new_folder_box.append(&new_folder_btn);

    popover_box.append(&search_entry);
    popover_box.append(&list_scroll);
    popover_box.append(&new_folder_box);
    folder_popover.set_child(Some(&popover_box));

    let folder_picker_btn = gtk::MenuButton::builder()
        .child(&folder_btn_content)
        .popover(&folder_popover)
        .hexpand(true)
        .build();

    let is_folder_action = matches!(initial_sel, 0 | 1);
    folder_picker_btn.set_visible(is_folder_action);
    target.set_visible(!is_folder_action);

    let collapsed_accounts: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(std::collections::HashSet::new()));
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild_folder_choices: Rc<dyn Fn()> = {
        let folder_list = folder_list.clone();
        let search_entry = search_entry.clone();
        let target = target.clone();
        let folder_popover = folder_popover.clone();
        let update_folder_label = update_folder_label.clone();
        let collapsed_accounts = Rc::clone(&collapsed_accounts);
        let rebuild_holder = Rc::clone(&rebuild_holder);
        Rc::new(move || {
            while let Some(child) = folder_list.first_child() {
                folder_list.remove(&child);
            }
            let raw_query = search_entry.text().trim().to_string();
            let query = raw_query.to_lowercase();
            let current = target.text().trim().to_string();
            let groups = collect_filter_folder_account_groups(&current);
            let is_searching = !query.is_empty();

            for group in groups {
                let matching_items: Vec<FilterFolderTreeItem> = group
                    .items
                    .into_iter()
                    .filter(|item| {
                        !is_searching
                            || item.full_search_text.to_lowercase().contains(&query)
                            || item.raw_target.to_lowercase().contains(&query)
                            || group.account_label.to_lowercase().contains(&query)
                    })
                    .collect();
                if matching_items.is_empty() {
                    continue;
                }

                let is_collapsed =
                    !is_searching && collapsed_accounts.borrow().contains(&group.account_id);

                // Account Tree Header Row
                let header_row = gtk::ListBoxRow::builder()
                    .selectable(false)
                    .activatable(false)
                    .build();
                let header_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .margin_top(4)
                    .margin_bottom(2)
                    .margin_start(4)
                    .margin_end(6)
                    .build();
                let chevron = gtk::Image::from_icon_name(if is_collapsed {
                    "pan-end-symbolic"
                } else {
                    "pan-down-symbolic"
                });
                chevron.set_pixel_size(14);
                chevron.add_css_class("dim-label");
                let acct_lbl = gtk::Label::builder()
                    .label(&group.account_label)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["heading", "caption"])
                    .build();
                header_box.append(&chevron);
                header_box.append(&acct_lbl);
                let header_btn = gtk::Button::builder()
                    .child(&header_box)
                    .css_classes(["flat"])
                    .hexpand(true)
                    .build();
                let acct_id = group.account_id.clone();
                let collapsed_c = Rc::clone(&collapsed_accounts);
                let rebuild_w = Rc::clone(&rebuild_holder);
                header_btn.connect_clicked(move |_| {
                    {
                        let mut set = collapsed_c.borrow_mut();
                        if !set.insert(acct_id.clone()) {
                            set.remove(&acct_id);
                        }
                    }
                    let cb = rebuild_w.borrow().clone();
                    if let Some(f) = cb {
                        f();
                    }
                });
                header_row.set_child(Some(&header_btn));
                folder_list.append(&header_row);

                if is_collapsed {
                    continue;
                }

                for item in matching_items {
                    let item_row = gtk::ListBoxRow::new();
                    let indent = 16 + (item.depth as i32 * 14);
                    let row_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(6)
                        .margin_top(3)
                        .margin_bottom(3)
                        .margin_start(indent)
                        .margin_end(6)
                        .build();
                    let icon = gtk::Image::builder()
                        .icon_name(item.icon_name)
                        .pixel_size(15)
                        .css_classes(["dim-label"])
                        .build();
                    let lbl = gtk::Label::builder()
                        .label(&item.leaf_label)
                        .halign(gtk::Align::Start)
                        .hexpand(true)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .build();
                    row_box.append(&icon);
                    row_box.append(&lbl);
                    if item.raw_target.eq_ignore_ascii_case(&current) {
                        row_box.append(&gtk::Image::from_icon_name("object-select-symbolic"));
                    }
                    let btn = gtk::Button::builder()
                        .child(&row_box)
                        .css_classes(["flat"])
                        .hexpand(true)
                        .build();
                    let target_c = target.clone();
                    let popover_c = folder_popover.clone();
                    let update_c = update_folder_label.clone();
                    let raw_c = item.raw_target.clone();
                    btn.connect_clicked(move |_| {
                        target_c.set_text(&raw_c);
                        update_c(&raw_c);
                        popover_c.popdown();
                    });
                    item_row.set_child(Some(&btn));
                    folder_list.append(&item_row);
                }
            }
        })
    };
    *rebuild_holder.borrow_mut() = Some(Rc::clone(&rebuild_folder_choices));

    {
        let rebuild = Rc::clone(&rebuild_folder_choices);
        search_entry.connect_search_changed(move |_| {
            rebuild();
        });
    }
    {
        let rebuild = Rc::clone(&rebuild_folder_choices);
        let search_entry = search_entry.clone();
        folder_popover.connect_show(move |_| {
            search_entry.set_text("");
            rebuild();
        });
    }
    {
        let new_folder_entry_c = new_folder_entry.clone();
        let target_c = target.clone();
        let popover_c = folder_popover.clone();
        let update_c = update_folder_label.clone();
        let rebuild = Rc::clone(&rebuild_folder_choices);
        let create_local = move || {
            let raw_name = new_folder_entry_c.text().trim().to_string();
            if raw_name.is_empty() {
                return;
            }
            if let Ok(created) =
                crate::services::mail_store_service::MailStoreService::new().create_local_folder(&raw_name)
            {
                let clean = created.strip_prefix("custom:").unwrap_or(&created);
                let new_target = format!("local:{clean}");
                target_c.set_text(&new_target);
                update_c(&new_target);
                new_folder_entry_c.set_text("");
                rebuild();
                popover_c.popdown();
            }
        };
        let create_for_btn = create_local.clone();
        new_folder_btn.connect_clicked(move |_| {
            create_for_btn();
        });
        new_folder_entry.connect_activate(move |_| {
            create_local();
        });
    }

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
    row.append(&folder_picker_btn);
    row.append(&target);
    row.append(&remove);
    container.append(&row);

    let target_for_type = target.clone();
    let folder_picker_for_type = folder_picker_btn.clone();
    let update_label_for_type = update_folder_label.clone();
    action_type.connect_selected_notify(move |dropdown| {
        let sel = dropdown.selected();
        let is_folder = matches!(sel, 0 | 1);
        let needs_target = matches!(sel, 0 | 1 | 12 | 13 | 14 | 16 | 17 | 19 | 20 | 21 | 22);
        folder_picker_for_type.set_visible(is_folder);
        target_for_type.set_visible(!is_folder);
        target_for_type.set_sensitive(needs_target);
        if needs_target && target_for_type.text().trim().is_empty() {
            let default_val = match sel {
                0 | 1 => "local:inbox",
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
        if is_folder {
            update_label_for_type(&target_for_type.text());
        }
        let placeholder = match sel {
            0 | 1 => gettext("Folder (e.g. archive, local:inbox, local:archive)"),
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
            .property("content-width", 680)
            .property("content-height", 560)
            .build()
    }

    pub fn new_for_edit(rule: &MailFilterRule) -> Self {
        let encoded = serde_json::to_string(rule).expect("filter rule should serialize");
        glib::Object::builder()
            .property("title", &gettext("Edit Filter Rule"))
            .property("content-width", 680)
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
        let group_general = adw::PreferencesGroup::new();

        let row_rulename = adw::EntryRow::builder()
            .title(gettext("Rule Name"))
            .use_markup(false)
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
            .use_markup(false)
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

        let match_mode_options = [
            gettext("Match all (AND)"),
            gettext("Match any (OR)"),
        ];
        let match_mode_slices: Vec<&str> = match_mode_options.iter().map(|s| s.as_str()).collect();
        let group_cond = adw::PreferencesGroup::new();
        group_cond.set_visible(false);
        let row_match_mode = adw::ComboRow::builder()
            .title(gettext("Condition Logic"))
            .use_markup(false)
            .model(&gtk::StringList::new(&match_mode_slices))
            .build();
        row_match_mode.add_prefix(&gtk::Image::from_icon_name("edit-select-all-symbolic"));
        if let Some(rule) = edit_rule.as_ref() {
            row_match_mode.set_selected(if rule.match_all { 0 } else { 1 });
        }
        group_cond.add(&row_match_mode);

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
                &group_cond,
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
            #[strong]
            group_cond,
            move |_| {
                append_condition_editor(
                    &condition_rows_box,
                    &condition_editors,
                    &next_condition_id,
                    &group_cond,
                    FilterCondition {
                        field: FilterField::Subject,
                        value: String::new(),
                        operator: FilterOperator::Contains,
                    },
                );
            }
        ));

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
