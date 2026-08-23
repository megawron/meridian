// SPDX-License-Identifier: GPL-3.0-or-later
//
// Advanced contact-search editor.  The dialog edits the portable typed query
// contract; it never constructs a GTK Expression or a backend S-expression.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::contact_search_service::{
    ContactClauseGroup, ContactOuterFilters, ContactSearchClause, ContactSearchField,
    ContactSearchOperator, ContactSearchQuery, operators_for_field, parse_query_text,
};

const FIELD_LABELS: [&str; 5] = ["Name", "E-Mail", "Telefon", "Kategorie", "Alle Felder"];

#[derive(Clone)]
pub struct ClauseRow {
    root: gtk::Box,
    field: gtk::DropDown,
    operator: gtk::DropDown,
    value: gtk::Entry,
}

mod imp {
    use super::*;

    pub struct ContactSearchDialog {
        pub rows: RefCell<Option<Rc<RefCell<Vec<ClauseRow>>>>>,
        pub rows_box: RefCell<Option<gtk::Box>>,
        pub group: RefCell<Option<gtk::DropDown>>,
        pub raw_query: RefCell<Option<gtk::Entry>>,
        pub category: RefCell<Option<gtk::Entry>>,
        pub unmatched_category: RefCell<Option<gtk::CheckButton>>,
        pub duplicate_only: RefCell<Option<gtk::CheckButton>>,
        pub status: RefCell<Option<gtk::Label>>,
        pub initial_filters: RefCell<ContactOuterFilters>,
        pub on_apply: RefCell<Option<Box<dyn Fn(ContactSearchQuery) + 'static>>>,
    }

    impl Default for ContactSearchDialog {
        fn default() -> Self {
            Self {
                rows: RefCell::new(None),
                rows_box: RefCell::new(None),
                group: RefCell::new(None),
                raw_query: RefCell::new(None),
                category: RefCell::new(None),
                unmatched_category: RefCell::new(None),
                duplicate_only: RefCell::new(None),
                status: RefCell::new(None),
                initial_filters: RefCell::new(ContactOuterFilters::default()),
                on_apply: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContactSearchDialog {
        const NAME: &'static str = "EvolutionContactSearchDialog";
        type Type = super::ContactSearchDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for ContactSearchDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for ContactSearchDialog {}
    impl WindowImpl for ContactSearchDialog {}
    impl AdwWindowImpl for ContactSearchDialog {}
}

glib::wrapper! {
    pub struct ContactSearchDialog(ObjectSubclass<imp::ContactSearchDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ContactSearchDialog {
    pub fn new() -> Self {
        Self::with_query(ContactSearchQuery::all(), |_| {})
    }

    pub fn with_query<F>(query: ContactSearchQuery, on_apply: F) -> Self
    where
        F: Fn(ContactSearchQuery) + 'static,
    {
        let dialog: Self = glib::Object::builder()
            .property("title", "Erweiterte Kontaktsuche")
            .property("modal", false)
            .property("default-width", 760)
            .property("default-height", 580)
            .build();
        dialog.imp().on_apply.replace(Some(Box::new(on_apply)));
        let query = seed_query_for_reopen(&query);
        dialog.configure_query(&query);
        dialog
    }

    fn build_ui(&self) {
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Erweiterte Kontaktsuche")
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
            .label("Die Bedingungen werden als portable Kontaktsuche ausgewertet. Quelle und Seitenfilter bleiben äußere Filter.")
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&intro);

        let group = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                ContactClauseGroup::All.label(),
                ContactClauseGroup::Any.label(),
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

        let rows_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let rows = Rc::new(RefCell::new(Vec::new()));
        self.imp().rows.replace(Some(Rc::clone(&rows)));
        self.imp().rows_box.replace(Some(rows_box.clone()));
        content.append(&rows_box);

        let add_clause = gtk::Button::builder()
            .label("Bedingung hinzufügen")
            .icon_name("list-add-symbolic")
            .halign(gtk::Align::Start)
            .css_classes(vec!["flat"])
            .build();
        let rows_for_add = Rc::clone(&rows);
        let rows_box_for_add = rows_box.clone();
        add_clause.connect_clicked(move |_| {
            append_clause_row(
                &rows_for_add,
                &rows_box_for_add,
                ContactSearchField::Name,
                ContactSearchOperator::Contains,
                "",
            );
        });
        content.append(&add_clause);

        let raw_query = gtk::Entry::builder()
            .placeholder_text(
                "Optional: name contains \"Ada Lovelace\" OR email is ada@example.test",
            )
            .hexpand(true)
            .build();
        let raw_row = adw::ActionRow::builder()
            .title("Portable Suchsyntax")
            .subtitle("Nur die typisierte Syntax wird akzeptiert; GTK3-Ausdrücke werden sichtbar abgelehnt.")
            .build();
        raw_row.add_suffix(&raw_query);
        content.append(&raw_row);
        self.imp().raw_query.replace(Some(raw_query));

        let category = gtk::Entry::builder()
            .placeholder_text("Keine äußere Kategorie")
            .hexpand(true)
            .build();
        let category_row = adw::ActionRow::builder()
            .title("Äußerer Kategoriefilter")
            .subtitle("Exakte Kategorie zusätzlich zu den Bedingungen")
            .build();
        category_row.add_suffix(&category);
        content.append(&category_row);
        self.imp().category.replace(Some(category));

        let unmatched_category = gtk::CheckButton::builder()
            .label("Nur Kontakte ohne Kategorie")
            .halign(gtk::Align::Start)
            .build();
        content.append(&unmatched_category);
        self.imp()
            .unmatched_category
            .replace(Some(unmatched_category));

        let duplicate_only = gtk::CheckButton::builder()
            .label("Nur Dubletten")
            .halign(gtk::Align::Start)
            .build();
        content.append(&duplicate_only);
        self.imp().duplicate_only.replace(Some(duplicate_only));

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
            move |_| dialog.close()
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

        toolbar.set_content(Some(&content));
        self.set_content(Some(&toolbar));

        append_clause_row(
            &rows,
            &rows_box,
            ContactSearchField::Name,
            ContactSearchOperator::Contains,
            "",
        );
    }

    fn configure_query(&self, query: &ContactSearchQuery) {
        self.imp().initial_filters.replace(query.filters.clone());
        if let Some(group) = self.imp().group.borrow().as_ref() {
            group.set_selected(match query.group {
                ContactClauseGroup::All => 0,
                ContactClauseGroup::Any => 1,
            });
        }
        if let Some(category) = self.imp().category.borrow().as_ref() {
            category.set_text(query.filters.category.as_deref().unwrap_or_default());
        }
        if let Some(unmatched) = self.imp().unmatched_category.borrow().as_ref() {
            unmatched.set_active(query.filters.unmatched_category);
        }
        if let Some(duplicate) = self.imp().duplicate_only.borrow().as_ref() {
            duplicate.set_active(query.filters.duplicate_only);
        }
        if let (Some(rows), Some(rows_box)) = (
            self.imp().rows.borrow().as_ref().cloned(),
            self.imp().rows_box.borrow().as_ref().cloned(),
        ) {
            for row in rows.borrow_mut().drain(..) {
                rows_box.remove(&row.root);
            }
            if !query.clauses.is_empty() {
                for clause in &query.clauses {
                    append_clause_row(
                        &rows,
                        &rows_box,
                        clause.field,
                        clause.operator,
                        &clause.value,
                    );
                }
            }
        }
        if let Some(raw) = self.imp().raw_query.borrow().as_ref() {
            raw.set_text("");
        }
    }

    fn apply_query(&self) {
        let query = match self.build_query() {
            Ok(query) => query,
            Err(error) => {
                self.set_error(&error.to_string());
                return;
            }
        };
        if let Some(callback) = self.imp().on_apply.borrow().as_ref() {
            callback(query);
        }
        self.close();
    }

    fn build_query(
        &self,
    ) -> Result<
        ContactSearchQuery,
        crate::services::contact_search_service::ContactSearchValidationError,
    > {
        let filters = self.imp().initial_filters.borrow().clone();
        let raw = self
            .imp()
            .raw_query
            .borrow()
            .as_ref()
            .map(|entry| entry.text().to_string())
            .unwrap_or_default();
        let mut query = if raw.trim().is_empty() {
            let group = self
                .imp()
                .group
                .borrow()
                .as_ref()
                .map(|group| {
                    if group.selected() == 1 {
                        ContactClauseGroup::Any
                    } else {
                        ContactClauseGroup::All
                    }
                })
                .unwrap_or_default();
            let rows = self
                .imp()
                .rows
                .borrow()
                .as_ref()
                .cloned()
                .unwrap_or_default();
            let clauses = rows
                .borrow()
                .iter()
                .map(|row| {
                    ContactSearchClause::new(
                        field_for_index(row.field.selected()),
                        operator_for_index(
                            row.operator.selected(),
                            field_for_index(row.field.selected()),
                        ),
                        row.value.text().to_string(),
                    )
                })
                .collect();
            ContactSearchQuery::new(group, clauses)
        } else {
            parse_query_text(&raw)?
        };
        let category = self
            .imp()
            .category
            .borrow()
            .as_ref()
            .map(|entry| entry.text().trim().to_string())
            .filter(|value| !value.is_empty());
        let unmatched_category = self
            .imp()
            .unmatched_category
            .borrow()
            .as_ref()
            .is_some_and(|check| check.is_active());
        let duplicate_only = self
            .imp()
            .duplicate_only
            .borrow()
            .as_ref()
            .is_some_and(|check| check.is_active());
        let mut updated_filters = filters;
        updated_filters.category = category;
        updated_filters.unmatched_category = unmatched_category;
        updated_filters.duplicate_only = duplicate_only;
        query.filters = updated_filters;
        query.validate()?;
        Ok(query)
    }

    fn set_error(&self, message: &str) {
        if let Some(status) = self.imp().status.borrow().as_ref() {
            status.set_text(message);
            status.remove_css_class("dim-label");
            status.add_css_class("error");
        }
    }
}

/// Capture the exact typed query for a later dialog instance.  Re-opening the
/// editor must preserve structured clauses and outer filters independently of
/// the widgets that happened to be visible in the previous instance.
pub(crate) fn seed_query_for_reopen(active_query: &ContactSearchQuery) -> ContactSearchQuery {
    active_query.clone()
}

fn append_clause_row(
    rows: &Rc<RefCell<Vec<ClauseRow>>>,
    rows_box: &gtk::Box,
    selected_field: ContactSearchField,
    selected_operator: ContactSearchOperator,
    value: &str,
) {
    let field_dropdown = gtk::DropDown::new(
        Some(gtk::StringList::new(&FIELD_LABELS)),
        None::<&gtk::Expression>,
    );
    field_dropdown.set_selected(field_index(selected_field));
    let operator_list_model = operator_model(selected_field);
    let operator_dropdown = gtk::DropDown::new(Some(operator_list_model), None::<&gtk::Expression>);
    operator_dropdown.set_selected(operator_index(selected_field, selected_operator));
    let value_entry = gtk::Entry::builder().text(value).hexpand(true).build();
    let remove = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .tooltip_text("Bedingung entfernen")
        .css_classes(vec!["flat"])
        .build();
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    root.append(&field_dropdown);
    root.append(&operator_dropdown);
    root.append(&value_entry);
    root.append(&remove);
    rows_box.append(&root);

    let row = ClauseRow {
        root: root.clone(),
        field: field_dropdown.clone(),
        operator: operator_dropdown.clone(),
        value: value_entry,
    };
    rows.borrow_mut().push(row);

    let rows_for_remove = Rc::clone(rows);
    let root_for_remove = root.clone();
    let rows_box_for_remove = rows_box.clone();
    remove.connect_clicked(move |_| {
        rows_for_remove
            .borrow_mut()
            .retain(|candidate| candidate.root != root_for_remove);
        rows_box_for_remove.remove(&root_for_remove);
    });

    let operator_for_field = operator_dropdown.clone();
    field_dropdown.connect_selected_notify(move |field| {
        let selected_field = field_for_index(field.selected());
        let model = operator_model(selected_field);
        operator_for_field.set_model(Some(&model));
        operator_for_field.set_selected(0);
    });
}

fn field_for_index(index: u32) -> ContactSearchField {
    match index {
        1 => ContactSearchField::Email,
        2 => ContactSearchField::Phone,
        3 => ContactSearchField::Category,
        4 => ContactSearchField::AnyField,
        _ => ContactSearchField::Name,
    }
}

fn field_index(field: ContactSearchField) -> u32 {
    match field {
        ContactSearchField::Name => 0,
        ContactSearchField::Email => 1,
        ContactSearchField::Phone => 2,
        ContactSearchField::Category => 3,
        ContactSearchField::AnyField => 4,
    }
}

fn operator_model(field: ContactSearchField) -> gtk::StringList {
    let labels = operators_for_field(field)
        .iter()
        .map(|operator| operator.label())
        .collect::<Vec<_>>();
    gtk::StringList::new(&labels)
}

fn operator_index(field: ContactSearchField, operator: ContactSearchOperator) -> u32 {
    operators_for_field(field)
        .iter()
        .position(|candidate| *candidate == operator)
        .unwrap_or(0) as u32
}

fn operator_for_index(index: u32, field: ContactSearchField) -> ContactSearchOperator {
    operators_for_field(field)
        .get(index as usize)
        .copied()
        .unwrap_or(ContactSearchOperator::Contains)
}

impl Default for ContactSearchDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::seed_query_for_reopen;
    use crate::services::contact_search_service::{
        ContactClauseGroup, ContactOuterFilters, ContactSearchClause, ContactSearchField,
        ContactSearchOperator, ContactSearchQuery,
    };

    #[test]
    fn re_open_is_seeded_from_the_current_active_structured_query() {
        let active = ContactSearchQuery::new(
            ContactClauseGroup::Any,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "Ada",
                ),
                ContactSearchClause::new(
                    ContactSearchField::Email,
                    ContactSearchOperator::EndsWith,
                    "example.test",
                ),
            ],
        )
        .with_filters(
            ContactOuterFilters::default()
                .source("book-a")
                .category("Work")
                .unmatched_category(true)
                .duplicate_only(true),
        );

        let mut reopened = seed_query_for_reopen(&active);
        assert_eq!(reopened, active);
        reopened.clauses[0].value = "Grace".to_string();
        assert_eq!(active.clauses[0].value, "Ada");
    }
}
