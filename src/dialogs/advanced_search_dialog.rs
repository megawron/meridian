// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable Advanced Search rule builder.
//
// Evolution's original advanced-search dialog builds an opaque EFilterRule
// and lets the mail shell compile that rule for the active backend.  This
// dialog deliberately emits only the provider-neutral mail-query syntax that
// the port can validate and evaluate on every supported platform.  A query is
// never sent to the callback until the canonical parser accepts it.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::search_engine::{MailSearchFieldScope, parse_mail_query};

fn text_field_labels() -> Vec<String> {
    vec![
        gettext("Subject"),
        gettext("Sender"),
        gettext("Recipients"),
        gettext("Message Body"),
        gettext("All Fields"),
    ]
}

fn text_operator_labels() -> Vec<String> {
    vec![
        gettext("contains"),
        gettext("is"),
        gettext("starts with"),
        gettext("ends with"),
    ]
}

fn state_labels() -> Vec<String> {
    vec![
        gettext("Unread"),
        gettext("Flagged"),
        gettext("Important"),
        gettext("Has Attachment"),
    ]
}

fn date_operator_labels() -> Vec<String> {
    vec![gettext("before"), gettext("after"), gettext("on")]
}

fn string_list_from_owned(items: &[String]) -> gtk::StringList {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    gtk::StringList::new(&refs)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuleKind {
    Text,
    State,
    Date,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupKind {
    All,
    Any,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextField {
    Subject,
    From,
    Recipients,
    Body,
    Any,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextMatcher {
    Contains,
    Exact,
    StartsWith,
    EndsWith,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SearchState {
    Unread,
    Flagged,
    Important,
    Attachment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DateOperator {
    Before,
    After,
    On,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuleSpec {
    Text {
        field: TextField,
        matcher: TextMatcher,
        value: String,
        negated: bool,
    },
    State {
        state: SearchState,
        negated: bool,
    },
    Date {
        operator: DateOperator,
        value: String,
        negated: bool,
    },
}

#[derive(Clone)]
pub(crate) struct RuleRow {
    id: u64,
    root: gtk::Box,
    kind: gtk::DropDown,
    selector: gtk::DropDown,
    operator: gtk::DropDown,
    value: gtk::Entry,
    negate: gtk::CheckButton,
}

mod imp {
    use super::*;

    pub struct AdvancedSearchDialog {
        pub(super) rows: RefCell<Option<Rc<RefCell<Vec<RuleRow>>>>>,
        pub(super) rows_box: RefCell<Option<gtk::Box>>,
        pub(super) next_row_id: Rc<Cell<u64>>,
        pub(super) group: RefCell<Option<gtk::DropDown>>,
        pub(super) negate_group: RefCell<Option<gtk::CheckButton>>,
        pub(super) raw_query: RefCell<Option<gtk::Entry>>,
        pub(super) status: RefCell<Option<gtk::Label>>,
        pub(super) on_apply: RefCell<Option<Box<dyn Fn(String) + 'static>>>,
    }

    impl Default for AdvancedSearchDialog {
        fn default() -> Self {
            Self {
                rows: RefCell::new(None),
                rows_box: RefCell::new(None),
                next_row_id: Rc::new(Cell::new(0)),
                group: RefCell::new(None),
                negate_group: RefCell::new(None),
                raw_query: RefCell::new(None),
                status: RefCell::new(None),
                on_apply: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AdvancedSearchDialog {
        const NAME: &'static str = "MeridianAdvancedSearchDialog";
        type Type = super::AdvancedSearchDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for AdvancedSearchDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for AdvancedSearchDialog {}
    impl AdwDialogImpl for AdvancedSearchDialog {}
}

glib::wrapper! {
    pub struct AdvancedSearchDialog(ObjectSubclass<imp::AdvancedSearchDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AdvancedSearchDialog {
    /// Create an empty advanced-search builder.
    pub fn new() -> Self {
        Self::with_query("", |_| {})
    }

    /// Create an advanced-search builder and deliver the validated portable
    /// query when the user presses Apply.  A non-empty `initial_query` is
    /// shown in the exact-syntax entry and remains lossless until the user
    /// clears that entry to use the visual rule builder.
    pub fn with_query<F>(initial_query: &str, on_apply: F) -> Self
    where
        F: Fn(String) + 'static,
    {
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Advanced Search"))
            .property("content-width", 820)
            .property("content-height", 620)
            .build();
        dialog.imp().on_apply.replace(Some(Box::new(on_apply)));
        dialog.configure_initial_query(initial_query);
        dialog
    }

    /// Convenience API for callers that already have a parent window.
    pub fn present<W: IsA<gtk::Widget>, F>(parent: &W, initial_query: &str, on_apply: F)
    where
        F: Fn(String) + 'static,
    {
        let dialog = Self::with_query(initial_query, on_apply);
        dialog.present(Some(parent));
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label(gettext("Advanced Search"))
                .css_classes(["title-4"])
                .build(),
        ));
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_top(14)
            .margin_bottom(14)
            .build();

        let intro = gtk::Label::builder()
            .label(gettext(
                "Conditions are combined into a portable search query. Unsupported \
                 expressions are rejected before applying.",
            ))
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label"])
            .build();
        content.append(&intro);

        let group_items = [
            gettext("Match all conditions (AND)"),
            gettext("Match any condition (OR)"),
        ];
        let group = gtk::DropDown::new(
            Some(string_list_from_owned(&group_items)),
            None::<&gtk::Expression>,
        );
        group.set_selected(0);
        group.set_tooltip_text(Some(&gettext("Combine conditions with AND or OR")));
        let group_row = adw::ActionRow::builder()
            .title(gettext("Match conditions"))
            .subtitle(gettext("All or any of the conditions"))
            .build();
        group_row.add_suffix(&group);
        content.append(&group_row);
        self.imp().group.replace(Some(group.clone()));

        let negate_group = gtk::CheckButton::builder()
            .label(gettext("Negate entire condition group"))
            .halign(gtk::Align::Start)
            .build();
        negate_group.set_tooltip_text(Some(&gettext("Produces not:(and:(…)) or not:(or:(…))")));
        content.append(&negate_group);
        self.imp().negate_group.replace(Some(negate_group.clone()));

        let rows_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let rows = Rc::new(RefCell::new(Vec::new()));
        let next_row_id = Rc::clone(&self.imp().next_row_id);
        self.imp().rows.replace(Some(Rc::clone(&rows)));
        self.imp().rows_box.replace(Some(rows_box.clone()));

        let rows_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let rows_title = gtk::Label::builder()
            .label(gettext("Conditions"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let add_row = gtk::Button::builder()
            .label(gettext("Add Condition"))
            .icon_name("list-add-symbolic")
            .css_classes(["flat"])
            .build();
        rows_header.append(&rows_title);
        rows_header.append(&add_row);
        content.append(&rows_header);
        content.append(&rows_box);

        let raw_query = gtk::Entry::builder()
            .placeholder_text(gettext(
                "Optional: and:(subject-contains:\"Project\" is:unread)",
            ))
            .hexpand(true)
            .build();
        raw_query.set_tooltip_text(Some(&gettext(
            "A custom portable query expression is validated and used directly; leave empty to use the visual condition builder.",
        )));
        let raw_row = adw::ActionRow::builder()
            .title(gettext("Portable Search Syntax"))
            .subtitle(gettext(
                "Custom expressions that cannot be represented as visual rules are edited here",
            ))
            .build();
        raw_row.add_suffix(&raw_query);
        content.append(&raw_row);
        self.imp().raw_query.replace(Some(raw_query.clone()));

        let raw_for_group = raw_query.downgrade();
        group.connect_selected_notify(move |_| {
            if let Some(raw) = raw_for_group.upgrade() {
                if !raw.text().is_empty() {
                    raw.set_text("");
                }
            }
        });
        let raw_for_negate_group = raw_query.downgrade();
        negate_group.connect_toggled(move |_| {
            if let Some(raw) = raw_for_negate_group.upgrade() {
                if !raw.text().is_empty() {
                    raw.set_text("");
                }
            }
        });

        let rows_for_add = Rc::clone(&rows);
        let rows_box_for_add = rows_box.clone();
        let next_row_id_for_add = Rc::clone(&next_row_id);
        let raw_for_add = raw_query.downgrade();
        add_row.connect_clicked(move |_| {
            if let Some(raw) = raw_for_add.upgrade() {
                if !raw.text().is_empty() {
                    raw.set_text("");
                }
            }
            append_rule_row(
                &rows_for_add,
                &rows_box_for_add,
                &next_row_id_for_add,
                raw_for_add.clone(),
            );
        });

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        content.append(&status);
        self.imp().status.replace(Some(status));

        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(4)
            .build();
        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        let apply = gtk::Button::builder()
            .label(gettext("Apply"))
            .css_classes(["suggested-action"])
            .build();
        apply.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.apply_query()
        ));
        actions.append(&cancel);
        actions.append(&apply);
        content.append(&actions);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content)
            .build();
        toolbar.set_content(Some(&scroll));
        self.set_child(Some(&toolbar));

        append_rule_row(&rows, &rows_box, &next_row_id, raw_query.downgrade());
    }

    fn configure_initial_query(&self, initial_query: &str) {
        let trimmed = initial_query.trim();
        if trimmed.is_empty() {
            if let Some(raw_query) = self.imp().raw_query.borrow().as_ref() {
                raw_query.set_text("");
            }
            return;
        }

        if let Some((group_kind, negate_group, parsed_rules)) = parse_visual_rules(trimmed) {
            if let Some(group) = self.imp().group.borrow().as_ref() {
                group.set_selected(match group_kind {
                    GroupKind::All => 0,
                    GroupKind::Any => 1,
                });
            }
            if let Some(negate_check) = self.imp().negate_group.borrow().as_ref() {
                negate_check.set_active(negate_group);
            }
            if let (Some(rows), Some(rows_box)) = (
                self.imp().rows.borrow().as_ref().cloned(),
                self.imp().rows_box.borrow().as_ref().cloned(),
            ) {
                let old_rows: Vec<RuleRow> = rows.borrow_mut().drain(..).collect();
                for old in old_rows {
                    rows_box.remove(&old.root);
                }
                let raw_weak = self
                    .imp()
                    .raw_query
                    .borrow()
                    .as_ref()
                    .map(|r| r.downgrade())
                    .unwrap_or_default();
                for spec in &parsed_rules {
                    let row = append_rule_row(
                        &rows,
                        &rows_box,
                        &self.imp().next_row_id,
                        raw_weak.clone(),
                    );
                    apply_rule_spec_to_row(&row, spec);
                }
            }
            if let Some(raw_query) = self.imp().raw_query.borrow().as_ref() {
                raw_query.set_text("");
            }
        } else if let Some(raw_query) = self.imp().raw_query.borrow().as_ref() {
            raw_query.set_text(trimmed);
        }
    }

    fn apply_query(&self) {
        let query = match self.build_query() {
            Ok(query) => query,
            Err(error) => {
                self.set_error(&error);
                return;
            }
        };

        let callback = self.imp().on_apply.borrow_mut().take();
        if let Some(callback) = callback {
            callback(query);
        }
        self.close();
    }

    fn build_query(&self) -> Result<String, String> {
        if let Some(raw_query) = self.imp().raw_query.borrow().as_ref() {
            let raw_query = raw_query.text().trim().to_string();
            if !raw_query.is_empty() {
                validate_query(&raw_query)?;
                return Ok(raw_query);
            }
        }

        let group = self
            .imp()
            .group
            .borrow()
            .as_ref()
            .map(|group| {
                if group.selected() == 1 {
                    GroupKind::Any
                } else {
                    GroupKind::All
                }
            })
            .unwrap_or(GroupKind::All);
        let negate_group = self
            .imp()
            .negate_group
            .borrow()
            .as_ref()
            .is_some_and(|check| check.is_active());
        let rows = self
            .imp()
            .rows
            .borrow()
            .as_ref()
            .cloned()
            .unwrap_or_default();
        let rules = rows
            .borrow()
            .iter()
            .map(rule_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        build_query(group, negate_group, &rules)
    }

    fn set_error(&self, message: &str) {
        if let Some(status) = self.imp().status.borrow().as_ref() {
            status.set_text(message);
            status.remove_css_class("dim-label");
            status.add_css_class("error");
        }
    }
}

impl Default for AdvancedSearchDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn append_rule_row(
    rows: &Rc<RefCell<Vec<RuleRow>>>,
    rows_box: &gtk::Box,
    next_row_id: &Rc<Cell<u64>>,
    raw_query: glib::WeakRef<gtk::Entry>,
) -> RuleRow {
    let id = next_row_id.get();
    next_row_id.set(id.saturating_add(1));

    let kind_items = [gettext("Text Field"), gettext("Status"), gettext("Date")];
    let kind = gtk::DropDown::new(
        Some(string_list_from_owned(&kind_items)),
        None::<&gtk::Expression>,
    );
    kind.set_selected(0);
    kind.set_width_request(110);
    kind.set_tooltip_text(Some(&gettext("Condition type")));

    let selector = gtk::DropDown::new(None::<gtk::StringList>, None::<&gtk::Expression>);
    selector.set_width_request(150);
    selector.set_tooltip_text(Some(&gettext("Select search field, status, or date")));

    let operator = gtk::DropDown::new(None::<gtk::StringList>, None::<&gtk::Expression>);
    operator.set_width_request(125);
    operator.set_tooltip_text(Some(&gettext("Select comparison operator")));

    let value = gtk::Entry::builder()
        .placeholder_text(gettext("Search value"))
        .hexpand(true)
        .build();
    let negate = gtk::CheckButton::builder()
        .label(gettext("not"))
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Negate this condition"))
        .build();
    let remove = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text(gettext("Remove condition"))
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    root.append(&kind);
    root.append(&selector);
    root.append(&operator);
    root.append(&value);
    root.append(&negate);
    root.append(&remove);
    rows_box.append(&root);

    let row = RuleRow {
        id,
        root: root.clone(),
        kind: kind.clone(),
        selector: selector.clone(),
        operator: operator.clone(),
        value: value.clone(),
        negate: negate.clone(),
    };
    refresh_rule_row(&row);
    rows.borrow_mut().push(row.clone());

    let selector_for_kind = selector.downgrade();
    let operator_for_kind = operator.downgrade();
    let value_for_kind = value.downgrade();
    let raw_for_kind = raw_query.clone();
    kind.connect_selected_notify(move |kind| {
        if let Some(raw) = raw_for_kind.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
        let Some(selector) = selector_for_kind.upgrade() else {
            return;
        };
        let Some(operator) = operator_for_kind.upgrade() else {
            return;
        };
        let Some(value) = value_for_kind.upgrade() else {
            return;
        };
        refresh_rule_widgets(kind, &selector, &operator, &value);
    });

    let raw_for_selector = raw_query.clone();
    selector.connect_selected_notify(move |_| {
        if let Some(raw) = raw_for_selector.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
    });
    let raw_for_operator = raw_query.clone();
    operator.connect_selected_notify(move |_| {
        if let Some(raw) = raw_for_operator.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
    });
    let raw_for_value = raw_query.clone();
    value.connect_changed(move |_| {
        if let Some(raw) = raw_for_value.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
    });
    let raw_for_negate = raw_query.clone();
    negate.connect_toggled(move |_| {
        if let Some(raw) = raw_for_negate.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
    });

    let rows_for_remove = Rc::downgrade(rows);
    let rows_box_for_remove = rows_box.downgrade();
    let kind_for_remove = kind.downgrade();
    let selector_for_remove = selector.downgrade();
    let operator_for_remove = operator.downgrade();
    let value_for_remove = value.downgrade();
    let negate_for_remove = negate.downgrade();
    let raw_for_remove = raw_query;
    remove.connect_clicked(move |_| {
        if let Some(raw) = raw_for_remove.upgrade() {
            if !raw.text().is_empty() {
                raw.set_text("");
            }
        }
        let Some(rows) = rows_for_remove.upgrade() else {
            return;
        };
        let removed = {
            let mut rows = rows.borrow_mut();
            if rows.len() > 1 {
                rows.iter()
                    .position(|candidate| candidate.id == id)
                    .map(|index| rows.remove(index))
            } else {
                None
            }
        };
        if let Some(removed) = removed {
            if let Some(rows_box) = rows_box_for_remove.upgrade() {
                rows_box.remove(&removed.root);
            }
        } else {
            if let Some(kind) = kind_for_remove.upgrade() {
                kind.set_selected(0);
            }
            if let Some(selector) = selector_for_remove.upgrade() {
                selector.set_selected(0);
            }
            if let Some(operator) = operator_for_remove.upgrade() {
                operator.set_selected(0);
            }
            if let Some(value) = value_for_remove.upgrade() {
                value.set_text("");
            }
            if let Some(negate) = negate_for_remove.upgrade() {
                negate.set_active(false);
            }
            // Keep the sole row in its canonical text-rule shape even when
            // `kind` was already zero and emitted no notify signal.
            if let (Some(kind), Some(selector), Some(operator), Some(value)) = (
                kind_for_remove.upgrade(),
                selector_for_remove.upgrade(),
                operator_for_remove.upgrade(),
                value_for_remove.upgrade(),
            ) {
                refresh_rule_widgets(&kind, &selector, &operator, &value);
            }
        }
    });

    row
}

fn apply_rule_spec_to_row(row: &RuleRow, spec: &RuleSpec) {
    match spec {
        RuleSpec::Text {
            field,
            matcher,
            value,
            negated,
        } => {
            row.kind.set_selected(0);
            refresh_rule_row(row);
            row.selector.set_selected(match field {
                TextField::Subject => 0,
                TextField::From => 1,
                TextField::Recipients => 2,
                TextField::Body => 3,
                TextField::Any => 4,
            });
            row.operator.set_selected(match matcher {
                TextMatcher::Contains => 0,
                TextMatcher::Exact => 1,
                TextMatcher::StartsWith => 2,
                TextMatcher::EndsWith => 3,
            });
            row.value.set_text(value);
            row.negate.set_active(*negated);
        }
        RuleSpec::State { state, negated } => {
            row.kind.set_selected(1);
            refresh_rule_row(row);
            row.selector.set_selected(match state {
                SearchState::Unread => 0,
                SearchState::Flagged => 1,
                SearchState::Important => 2,
                SearchState::Attachment => 3,
            });
            row.operator.set_selected(0);
            row.value.set_text("");
            row.negate.set_active(*negated);
        }
        RuleSpec::Date {
            operator,
            value,
            negated,
        } => {
            row.kind.set_selected(2);
            refresh_rule_row(row);
            row.selector.set_selected(0);
            row.operator.set_selected(match operator {
                DateOperator::Before => 0,
                DateOperator::After => 1,
                DateOperator::On => 2,
            });
            row.value.set_text(value);
            row.negate.set_active(*negated);
        }
    }
}

fn refresh_rule_row(row: &RuleRow) {
    refresh_rule_widgets(&row.kind, &row.selector, &row.operator, &row.value);
}

fn refresh_rule_widgets(
    kind: &gtk::DropDown,
    selector: &gtk::DropDown,
    operator: &gtk::DropDown,
    value: &gtk::Entry,
) {
    match rule_kind_from_index(kind.selected()) {
        RuleKind::Text => {
            let fields = string_list_from_owned(&text_field_labels());
            let operators = string_list_from_owned(&text_operator_labels());
            selector.set_model(Some(&fields));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_sensitive(true);
            value.set_placeholder_text(Some(&gettext("Search value")));
        }
        RuleKind::State => {
            let states = string_list_from_owned(&state_labels());
            let is_ops = [gettext("is")];
            let operators = string_list_from_owned(&is_ops);
            selector.set_model(Some(&states));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_text("");
            value.set_sensitive(false);
            value.set_placeholder_text(Some(&gettext("No value required")));
        }
        RuleKind::Date => {
            let date_fields = [gettext("Message Date")];
            let date = string_list_from_owned(&date_fields);
            let operators = string_list_from_owned(&date_operator_labels());
            selector.set_model(Some(&date));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_sensitive(true);
            value.set_placeholder_text(Some(&gettext("YYYY-MM-DD, today, or 7d")));
        }
    }
}

fn rule_from_row(row: &RuleRow) -> Result<RuleSpec, String> {
    let negated = row.negate.is_active();
    match rule_kind_from_index(row.kind.selected()) {
        RuleKind::Text => Ok(RuleSpec::Text {
            field: text_field_from_index(row.selector.selected()),
            matcher: text_matcher_from_index(row.operator.selected()),
            value: row.value.text().to_string(),
            negated,
        }),
        RuleKind::State => Ok(RuleSpec::State {
            state: state_from_index(row.selector.selected()),
            negated,
        }),
        RuleKind::Date => Ok(RuleSpec::Date {
            operator: date_operator_from_index(row.operator.selected()),
            value: row.value.text().to_string(),
            negated,
        }),
    }
}

fn rule_kind_from_index(index: u32) -> RuleKind {
    match index {
        1 => RuleKind::State,
        2 => RuleKind::Date,
        _ => RuleKind::Text,
    }
}

fn text_field_from_index(index: u32) -> TextField {
    match index {
        1 => TextField::From,
        2 => TextField::Recipients,
        3 => TextField::Body,
        4 => TextField::Any,
        _ => TextField::Subject,
    }
}

fn text_matcher_from_index(index: u32) -> TextMatcher {
    match index {
        1 => TextMatcher::Exact,
        2 => TextMatcher::StartsWith,
        3 => TextMatcher::EndsWith,
        _ => TextMatcher::Contains,
    }
}

fn state_from_index(index: u32) -> SearchState {
    match index {
        1 => SearchState::Flagged,
        2 => SearchState::Important,
        3 => SearchState::Attachment,
        _ => SearchState::Unread,
    }
}

fn date_operator_from_index(index: u32) -> DateOperator {
    match index {
        1 => DateOperator::After,
        2 => DateOperator::On,
        _ => DateOperator::Before,
    }
}

fn build_query(group: GroupKind, negate_group: bool, rules: &[RuleSpec]) -> Result<String, String> {
    if rules.is_empty() {
        return Err(gettext("At least one search condition is required"));
    }

    let expressions = rules
        .iter()
        .map(rule_expression)
        .collect::<Result<Vec<_>, _>>()?;
    let body = expressions.join(" ");
    let grouped = match group {
        GroupKind::All => format!("and:({body})"),
        GroupKind::Any => format!("or:({body})"),
    };
    let query = if negate_group {
        format!("not:({grouped})")
    } else {
        grouped
    };
    validate_query(&query)?;
    Ok(query)
}

fn rule_expression(rule: &RuleSpec) -> Result<String, String> {
    match rule {
        RuleSpec::Text {
            field,
            matcher,
            value,
            negated,
        } => {
            let value = quote_query_value(value)?;
            let expression = match (field, matcher) {
                (TextField::Any, matcher) => format!("any-{}:{value}", matcher.key()),
                (field, matcher) => {
                    format!("{}-{}:{value}", field.key(), matcher.key())
                }
            };
            Ok(negate_expression(expression, *negated))
        }
        RuleSpec::State { state, negated } => {
            Ok(negate_expression(format!("is:{}", state.key()), *negated))
        }
        RuleSpec::Date {
            operator,
            value,
            negated,
        } => {
            let value = quote_query_value(value)?;
            Ok(negate_expression(
                format!("{}:{value}", operator.key()),
                *negated,
            ))
        }
    }
}

fn negate_expression(expression: String, negated: bool) -> String {
    if negated {
        format!("not:({expression})")
    } else {
        expression
    }
}

fn quote_query_value(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(gettext("This condition requires a search value"));
    }
    if value
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err(gettext("Search values cannot contain line breaks"));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn validate_query(query: &str) -> Result<(), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err(gettext("Search query cannot be empty"));
    }
    let parsed = parse_mail_query(query, MailSearchFieldScope::Any)
        .map_err(|error| format!("{}: {error}", gettext("Invalid search query")))?;
    if parsed.is_none() {
        return Err(gettext("Search query cannot be empty"));
    }
    Ok(())
}

fn unwrap_prefix_parens<'a>(input: &'a str, prefix_with_open_paren: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    if trimmed.len() <= prefix_with_open_paren.len() || !trimmed.ends_with(')') {
        return None;
    }
    let head = trimmed.get(..prefix_with_open_paren.len())?;
    if !head.eq_ignore_ascii_case(prefix_with_open_paren) {
        return None;
    }
    // Verify that the opening parenthesis at `prefix_with_open_paren.len() - 1`
    // closes at the very last byte of `trimmed`.
    let after_open = &trimmed[prefix_with_open_paren.len()..];
    let mut depth = 1usize;
    let mut in_quotes = false;
    let mut escaped = false;
    for (byte_idx, ch) in after_open.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    if byte_idx + ch.len_utf8() == after_open.len() {
                        return Some(after_open[..byte_idx].trim());
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_rule_tokens(input: &str) -> Option<Vec<&str>> {
    let mut tokens = Vec::new();
    let mut token_start: Option<usize> = None;
    let mut in_quotes = false;
    let mut escaped = false;
    let mut paren_depth = 0usize;

    for (idx, ch) in input.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
            continue;
        }
        match ch {
            '"' => {
                if token_start.is_none() {
                    token_start = Some(idx);
                }
                in_quotes = true;
            }
            '(' => {
                if token_start.is_none() {
                    token_start = Some(idx);
                }
                paren_depth += 1;
            }
            ')' => {
                if paren_depth == 0 {
                    return None;
                }
                paren_depth -= 1;
            }
            c if c.is_whitespace() && paren_depth == 0 => {
                if let Some(start) = token_start.take() {
                    tokens.push(&input[start..idx]);
                }
            }
            _ => {
                if token_start.is_none() {
                    token_start = Some(idx);
                }
            }
        }
    }

    if in_quotes || paren_depth != 0 {
        return None;
    }
    if let Some(start) = token_start {
        tokens.push(&input[start..]);
    }
    if tokens.is_empty() {
        return None;
    }
    Some(tokens)
}

fn unquote_query_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('"') {
        if raw.len() < 2 || !raw.ends_with('"') {
            return None;
        }
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                // Unescaped quote inside indicates multiple quoted segments
                return None;
            } else {
                out.push(ch);
            }
        }
        if escaped || out.trim().is_empty() {
            return None;
        }
        Some(out)
    } else {
        if raw.chars().any(|c| c.is_whitespace() || c == '"' || c == '(' || c == ')') {
            return None;
        }
        Some(raw.to_string())
    }
}

fn parse_single_rule_spec(token: &str) -> Option<RuleSpec> {
    let (negated, core) = if let Some(inner) = unwrap_prefix_parens(token, "not:(") {
        (true, inner)
    } else {
        (false, token.trim())
    };
    let (lhs, rhs) = core.split_once(':')?;
    let lhs_lower = lhs.trim().to_ascii_lowercase();
    if lhs_lower == "is" {
        let state_val = unquote_query_value(rhs)?.to_ascii_lowercase();
        let state = match state_val.as_str() {
            "unread" => SearchState::Unread,
            "flagged" => SearchState::Flagged,
            "important" => SearchState::Important,
            "attachment" => SearchState::Attachment,
            _ => return None,
        };
        return Some(RuleSpec::State { state, negated });
    }

    if let Some(operator) = match lhs_lower.as_str() {
        "before" => Some(DateOperator::Before),
        "after" => Some(DateOperator::After),
        "on" => Some(DateOperator::On),
        _ => None,
    } {
        let value = unquote_query_value(rhs)?;
        return Some(RuleSpec::Date {
            operator,
            value,
            negated,
        });
    }

    let (field, suffix) = if let Some(rest) = lhs_lower.strip_prefix("subject") {
        (TextField::Subject, rest)
    } else if let Some(rest) = lhs_lower.strip_prefix("from") {
        (TextField::From, rest)
    } else if let Some(rest) = lhs_lower.strip_prefix("recipients") {
        (TextField::Recipients, rest)
    } else if let Some(rest) = lhs_lower.strip_prefix("body") {
        (TextField::Body, rest)
    } else if let Some(rest) = lhs_lower.strip_prefix("any") {
        (TextField::Any, rest)
    } else {
        return None;
    };

    let matcher = match suffix {
        "" | "-contains" => TextMatcher::Contains,
        "-matches" => TextMatcher::Exact,
        "-starts-with" => TextMatcher::StartsWith,
        "-ends-with" => TextMatcher::EndsWith,
        _ => return None,
    };
    let value = unquote_query_value(rhs)?;
    Some(RuleSpec::Text {
        field,
        matcher,
        value,
        negated,
    })
}

fn parse_visual_rules(query: &str) -> Option<(GroupKind, bool, Vec<RuleSpec>)> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    validate_query(trimmed).ok()?;

    let (group, negate_group, body) = if let Some(not_inner) = unwrap_prefix_parens(trimmed, "not:(")
    {
        if let Some(and_body) = unwrap_prefix_parens(not_inner, "and:(") {
            (GroupKind::All, true, and_body)
        } else if let Some(or_body) = unwrap_prefix_parens(not_inner, "or:(") {
            (GroupKind::Any, true, or_body)
        } else {
            (GroupKind::All, false, trimmed)
        }
    } else if let Some(and_body) = unwrap_prefix_parens(trimmed, "and:(") {
        (GroupKind::All, false, and_body)
    } else if let Some(or_body) = unwrap_prefix_parens(trimmed, "or:(") {
        (GroupKind::Any, false, or_body)
    } else {
        (GroupKind::All, false, trimmed)
    };

    let tokens = split_top_level_rule_tokens(body)?;
    let mut rules = Vec::with_capacity(tokens.len());
    for token in tokens {
        rules.push(parse_single_rule_spec(token)?);
    }
    if rules.is_empty() {
        return None;
    }
    Some((group, negate_group, rules))
}

impl TextMatcher {
    fn key(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Exact => "matches",
            Self::StartsWith => "starts-with",
            Self::EndsWith => "ends-with",
        }
    }
}

impl TextField {
    fn key(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::From => "from",
            Self::Recipients => "recipients",
            Self::Body => "body",
            Self::Any => "any",
        }
    }
}

impl SearchState {
    fn key(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Flagged => "flagged",
            Self::Important => "important",
            Self::Attachment => "attachment",
        }
    }
}

impl DateOperator {
    fn key(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
            Self::On => "on",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_escaped_text_terms_and_validates_them() {
        let query = build_query(
            GroupKind::All,
            false,
            &[RuleSpec::Text {
                field: TextField::Subject,
                matcher: TextMatcher::Contains,
                value: r#"A "quote" \\path"#.to_string(),
                negated: false,
            }],
        )
        .expect("escaped text should produce a valid query");

        assert_eq!(query, r#"and:(subject-contains:"A \"quote\" \\\\path")"#);
        assert!(validate_query(&query).is_ok());
    }

    #[test]
    fn emits_and_or_and_nested_negation_without_broadening() {
        let query = build_query(
            GroupKind::Any,
            true,
            &[
                RuleSpec::Text {
                    field: TextField::Subject,
                    matcher: TextMatcher::Exact,
                    value: "roadmap".to_string(),
                    negated: false,
                },
                RuleSpec::State {
                    state: SearchState::Unread,
                    negated: true,
                },
            ],
        )
        .expect("AND/OR/NOT query should validate");

        assert_eq!(
            query,
            r#"not:(or:(subject-matches:"roadmap" not:(is:unread)))"#
        );
        assert!(validate_query(&query).is_ok());
    }

    #[test]
    fn emits_all_supported_date_operators() {
        let query = build_query(
            GroupKind::All,
            false,
            &[
                RuleSpec::Date {
                    operator: DateOperator::Before,
                    value: "2026-08-01".to_string(),
                    negated: false,
                },
                RuleSpec::Date {
                    operator: DateOperator::After,
                    value: "7d".to_string(),
                    negated: false,
                },
                RuleSpec::Date {
                    operator: DateOperator::On,
                    value: "today".to_string(),
                    negated: false,
                },
            ],
        )
        .expect("date query should validate");

        assert_eq!(query, r#"and:(before:"2026-08-01" after:"7d" on:"today")"#);
    }

    #[test]
    fn any_field_matchers_expand_to_explicit_portable_fields() {
        let query = build_query(
            GroupKind::All,
            false,
            &[RuleSpec::Text {
                field: TextField::Any,
                matcher: TextMatcher::EndsWith,
                value: "example.org".to_string(),
                negated: false,
            }],
        )
        .expect("any-field query should validate");

        assert_eq!(query, r#"and:(any-ends-with:"example.org")"#);
    }

    #[test]
    fn leading_hyphen_in_any_field_is_a_literal_not_negation() {
        let query = build_query(
            GroupKind::All,
            false,
            &[RuleSpec::Text {
                field: TextField::Any,
                matcher: TextMatcher::Contains,
                value: "-foo".to_string(),
                negated: false,
            }],
        )
        .expect("leading hyphen should stay a literal field value");

        assert_eq!(query, r#"and:(any-contains:"-foo")"#);
        let parsed = parse_mail_query(&query, MailSearchFieldScope::Any)
            .expect("query should parse")
            .expect("query should not be empty");
        let fixture = crate::services::search_engine::encode_mail_query_fixture(&parsed)
            .expect("query should encode");
        let fixture: serde_json::Value =
            serde_json::from_str(&fixture).expect("fixture should be JSON");
        assert_eq!(fixture["query"]["value"], "-foo");
        assert_eq!(fixture["query"]["negated"], false);
    }

    #[test]
    fn invalid_and_empty_values_fail_closed() {
        assert!(quote_query_value("   ").is_err());
        assert!(quote_query_value("line\nbreak").is_err());
        assert!(validate_query("unknown:value").is_err());
        assert!(validate_query("not:(or:(subject:one)").is_err());
        assert!(build_query(GroupKind::All, false, &[]).is_err());
    }

    #[test]
    fn visual_rules_round_trip_losslessly() {
        let rules = vec![
            RuleSpec::Text {
                field: TextField::From,
                matcher: TextMatcher::Contains,
                value: "Jan".to_string(),
                negated: false,
            },
            RuleSpec::Text {
                field: TextField::Subject,
                matcher: TextMatcher::Contains,
                value: r#"Q4 "Report" \ final"#.to_string(),
                negated: true,
            },
            RuleSpec::State {
                state: SearchState::Unread,
                negated: false,
            },
            RuleSpec::Date {
                operator: DateOperator::After,
                value: "7d".to_string(),
                negated: true,
            },
        ];
        let query = build_query(GroupKind::All, false, &rules).expect("query should build");
        let (parsed_group, parsed_negate, parsed_rules) =
            parse_visual_rules(&query).expect("visual query should parse back into rules");
        assert_eq!(parsed_group, GroupKind::All);
        assert!(!parsed_negate);
        assert_eq!(parsed_rules, rules);

        let negated_or_query =
            build_query(GroupKind::Any, true, &rules).expect("negated OR query should build");
        let (parsed_group2, parsed_negate2, parsed_rules2) = parse_visual_rules(&negated_or_query)
            .expect("negated OR query should parse back into rules");
        assert_eq!(parsed_group2, GroupKind::Any);
        assert!(parsed_negate2);
        assert_eq!(parsed_rules2, rules);
    }
}
