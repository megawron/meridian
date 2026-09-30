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

use crate::services::search_engine::{MailSearchFieldScope, parse_mail_query};

const TEXT_FIELD_LABELS: &[&str] = &[
    "Betreff",
    "Absender",
    "Empfänger",
    "Nachrichteninhalt",
    "Alle Felder",
];
const TEXT_OPERATOR_LABELS: &[&str] = &["enthält", "ist", "beginnt mit", "endet mit"];
const STATE_LABELS: &[&str] = &["Ungelesen", "Markiert", "Wichtig", "Anhang vorhanden"];
const DATE_OPERATOR_LABELS: &[&str] = &["vor", "nach", "am"];

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
            .property("title", "Erweiterte Suche")
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
                .label("Erweiterte Suche")
                .css_classes(vec!["title-4"])
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
            .label(
                "Bedingungen werden als portable Suchabfrage verbunden. Nicht unterstützte \
                 Ausdrücke werden vor dem Anwenden sichtbar abgewiesen.",
            )
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&intro);

        let group = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Alle Bedingungen (UND)",
                "Mindestens eine Bedingung (ODER)",
            ])),
            None::<&gtk::Expression>,
        );
        group.set_selected(0);
        group.set_tooltip_text(Some("Bedingungen mit UND oder ODER verbinden"));
        let group_row = adw::ActionRow::builder()
            .title("Bedingungen verbinden")
            .subtitle("Alle oder mindestens eine Bedingung")
            .build();
        group_row.add_suffix(&group);
        content.append(&group_row);
        self.imp().group.replace(Some(group));

        let negate_group = gtk::CheckButton::builder()
            .label("Gesamte Bedingungsgruppe verneinen")
            .halign(gtk::Align::Start)
            .build();
        negate_group.set_tooltip_text(Some("Erzeugt not:(and:(…)) oder not:(or:(…))"));
        content.append(&negate_group);
        self.imp().negate_group.replace(Some(negate_group));

        let rows_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let rows = Rc::new(RefCell::new(Vec::new()));
        let next_row_id = Rc::new(Cell::new(0));
        self.imp().rows.replace(Some(Rc::clone(&rows)));
        self.imp().rows_box.replace(Some(rows_box.clone()));

        let rows_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let rows_title = gtk::Label::builder()
            .label("Bedingungen")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["heading"])
            .build();
        let add_row = gtk::Button::builder()
            .label("Bedingung hinzufügen")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["flat"])
            .build();
        rows_header.append(&rows_title);
        rows_header.append(&add_row);
        content.append(&rows_header);
        content.append(&rows_box);

        let rows_for_add = Rc::clone(&rows);
        let rows_box_for_add = rows_box.clone();
        let next_row_id_for_add = Rc::clone(&next_row_id);
        add_row.connect_clicked(move |_| {
            append_rule_row(&rows_for_add, &rows_box_for_add, &next_row_id_for_add);
        });

        let raw_query = gtk::Entry::builder()
            .placeholder_text("Optional: and:(subject-contains:\"Projekt\" is:unread)")
            .hexpand(true)
            .build();
        raw_query.set_tooltip_text(Some(
            "Ein vorhandener portabler Ausdruck wird unverändert validiert und verwendet; zum Rule-Builder leeren.",
        ));
        let raw_row = adw::ActionRow::builder()
            .title("Portable Suchsyntax")
            .subtitle("Exakte vorhandene Ausdrücke bleiben beim Bearbeiten erhalten")
            .build();
        raw_row.add_suffix(&raw_query);
        content.append(&raw_row);
        self.imp().raw_query.replace(Some(raw_query));

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&status);
        self.imp().status.replace(Some(status));

        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .margin_top(4)
            .build();
        let cancel = gtk::Button::with_label("Abbrechen");
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        let apply = gtk::Button::builder()
            .label("Anwenden")
            .css_classes(vec!["suggested-action"])
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

        append_rule_row(&rows, &rows_box, &next_row_id);
    }

    fn configure_initial_query(&self, initial_query: &str) {
        if let Some(raw_query) = self.imp().raw_query.borrow().as_ref() {
            raw_query.set_text(initial_query.trim());
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
) {
    let id = next_row_id.get();
    next_row_id.set(id.saturating_add(1));

    let kind = gtk::DropDown::new(
        Some(gtk::StringList::new(&["Textfeld", "Zustand", "Datum"])),
        None::<&gtk::Expression>,
    );
    kind.set_selected(0);
    kind.set_width_request(110);
    kind.set_tooltip_text(Some("Art der Suchbedingung"));

    let selector = gtk::DropDown::new(None::<gtk::StringList>, None::<&gtk::Expression>);
    selector.set_width_request(150);
    selector.set_tooltip_text(Some("Suchfeld, Zustand oder Datum auswählen"));

    let operator = gtk::DropDown::new(None::<gtk::StringList>, None::<&gtk::Expression>);
    operator.set_width_request(125);
    operator.set_tooltip_text(Some("Vergleichsoperator auswählen"));

    let value = gtk::Entry::builder()
        .placeholder_text("Suchwert")
        .hexpand(true)
        .build();
    let negate = gtk::CheckButton::builder()
        .label("nicht")
        .valign(gtk::Align::Center)
        .tooltip_text("Diese Bedingung verneinen")
        .build();
    let remove = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text("Bedingung entfernen")
        .css_classes(vec!["flat"])
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
    kind.connect_selected_notify(move |kind| {
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

    let rows_for_remove = Rc::downgrade(rows);
    let rows_box_for_remove = rows_box.downgrade();
    let kind_for_remove = kind.downgrade();
    let selector_for_remove = selector.downgrade();
    let operator_for_remove = operator.downgrade();
    let value_for_remove = value.downgrade();
    let negate_for_remove = negate.downgrade();
    remove.connect_clicked(move |_| {
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
            let fields = gtk::StringList::new(TEXT_FIELD_LABELS);
            let operators = gtk::StringList::new(TEXT_OPERATOR_LABELS);
            selector.set_model(Some(&fields));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_sensitive(true);
            value.set_placeholder_text(Some("Suchwert"));
        }
        RuleKind::State => {
            let states = gtk::StringList::new(STATE_LABELS);
            let operators = gtk::StringList::new(&["ist"]);
            selector.set_model(Some(&states));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_text("");
            value.set_sensitive(false);
            value.set_placeholder_text(Some("Kein Wert erforderlich"));
        }
        RuleKind::Date => {
            let date = gtk::StringList::new(&["Nachrichtendatum"]);
            let operators = gtk::StringList::new(DATE_OPERATOR_LABELS);
            selector.set_model(Some(&date));
            operator.set_model(Some(&operators));
            selector.set_selected(0);
            operator.set_selected(0);
            value.set_sensitive(true);
            value.set_placeholder_text(Some("YYYY-MM-DD, heute oder 7d"));
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
        return Err("Mindestens eine Suchbedingung ist erforderlich".to_string());
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
        return Err("Diese Bedingung benötigt einen Suchwert".to_string());
    }
    if value
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err("Suchwerte dürfen keine Zeilenumbrüche enthalten".to_string());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn validate_query(query: &str) -> Result<(), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Die Suchabfrage darf nicht leer sein".to_string());
    }
    let parsed = parse_mail_query(query, MailSearchFieldScope::Any)
        .map_err(|error| format!("Ungültige Suchabfrage: {error}"))?;
    if parsed.is_none() {
        return Err("Die Suchabfrage darf nicht leer sein".to_string());
    }
    Ok(())
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
}
