// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::contact_export_service::{ContactExportFormat, export_contacts};
use crate::services::contact_print_service::{
    build_contact_list_print_document, build_contact_print_document,
};
use crate::services::contact_search_service::{
    ContactIdentity, ContactOuterFilters, ContactSearchEvaluation, ContactSearchField,
    ContactSearchQuery, ContactSearchSnapshot, LastValidContactSearch, LatestContactSearchWorker,
    LatestGenerationEvaluator, field_values,
};
use crate::services::ebook_service::{
    EBookChangeKind, EBookContactRecord, EBookDataOrigin, EBookService, EBookSyncEvent,
    load_contacts_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;
use crate::services::vcard_service::{VCardContact, VCardListMemberKind};
use crate::views::accessibility::set_accessible_label;
use crate::views::set_button_icon_with_fallback;

const LOCAL_ADDRESS_BOOK_SOURCE_UID: &str = "local-addressbook";

#[derive(Debug)]
struct ContactPageSearchState {
    visible_identities: RefCell<HashSet<ContactIdentity>>,
    active_query: RefCell<ContactSearchQuery>,
    last_valid: RefCell<LastValidContactSearch>,
    generations: LatestGenerationEvaluator,
    worker: LatestContactSearchWorker,
    quick_field: Cell<ContactSearchField>,
}

impl Default for ContactPageSearchState {
    fn default() -> Self {
        Self {
            visible_identities: RefCell::new(HashSet::new()),
            active_query: RefCell::new(ContactSearchQuery::all()),
            last_valid: RefCell::new(LastValidContactSearch::default()),
            generations: LatestGenerationEvaluator::default(),
            worker: LatestContactSearchWorker::default(),
            quick_field: Cell::new(ContactSearchField::AnyField),
        }
    }
}

impl ContactPageSearchState {
    fn quick_field(&self) -> ContactSearchField {
        self.quick_field.get()
    }

    fn set_quick_field(&self, field: ContactSearchField) {
        self.quick_field.set(field);
    }

    fn active_query(&self) -> ContactSearchQuery {
        self.active_query.borrow().clone()
    }

    fn set_active_query(&self, query: ContactSearchQuery) {
        self.active_query.replace(query);
    }

    fn active_query_with_filters(&self, filters: ContactOuterFilters) -> ContactSearchQuery {
        let mut query = self.active_query();
        query.filters = filters;
        self.set_active_query(query.clone());
        query
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ContactTableColumn {
    #[default]
    Name,
    Company,
    Title,
    Email,
    Phone,
    Categories,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedContactTableRow {
    pub identity: ContactIdentity,
    pub name: String,
    pub company: String,
    pub title: String,
    pub email: String,
    pub phone: String,
    pub categories: Vec<String>,
    pub selected: bool,
    pub y: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreparedContactTableLayout {
    pub rows: Vec<PreparedContactTableRow>,
    pub width: f64,
    pub height: f64,
    pub row_height: f64,
    pub col_name_w: f64,
    pub col_company_w: f64,
    pub col_title_w: f64,
    pub col_email_w: f64,
    pub col_phone_w: f64,
    pub col_categories_w: f64,
}

pub(crate) fn prepare_contact_table_layout(
    contacts: &[EBookContactRecord],
    visible_filter: Option<&HashSet<ContactIdentity>>,
    selected_identities: &HashSet<ContactIdentity>,
    sort_col: ContactTableColumn,
    sort_desc: bool,
    width: f64,
) -> PreparedContactTableLayout {
    let row_height = 32.0;
    let mut filtered_contacts: Vec<&EBookContactRecord> = contacts
        .iter()
        .filter(|rec| {
            if let Some(visible) = visible_filter {
                if !visible.is_empty() {
                    return visible.contains(&ContactIdentity::new(&rec.source_uid, &rec.uid));
                }
            }
            true
        })
        .collect();

    filtered_contacts.sort_by(|a, b| {
        let ord = match sort_col {
            ContactTableColumn::Name => a.contact.fn_name.cmp(&b.contact.fn_name),
            ContactTableColumn::Company => a.contact.org.cmp(&b.contact.org),
            ContactTableColumn::Title => a.contact.title.cmp(&b.contact.title),
            ContactTableColumn::Email => a.contact.email.cmp(&b.contact.email),
            ContactTableColumn::Phone => a.contact.tel.cmp(&b.contact.tel),
            ContactTableColumn::Categories => {
                let cat_a = a.contact.categories.join(", ");
                let cat_b = b.contact.categories.join(", ");
                cat_a.cmp(&cat_b)
            }
        };
        if sort_desc {
            ord.reverse()
        } else {
            ord
        }
    });

    let mut rows = Vec::with_capacity(filtered_contacts.len());
    for (i, rec) in filtered_contacts.iter().enumerate() {
        let identity = ContactIdentity::new(&rec.source_uid, &rec.uid);
        let selected = selected_identities.contains(&identity);
        rows.push(PreparedContactTableRow {
            identity,
            name: rec.contact.fn_name.clone(),
            company: rec.contact.org.clone(),
            title: rec.contact.title.clone(),
            email: rec.contact.email.clone(),
            phone: rec.contact.tel.clone(),
            categories: rec.contact.categories.clone(),
            selected,
            y: (i as f64) * row_height,
            height: row_height,
        });
    }

    let min_width = 800.0;
    let available_w = width.max(min_width);
    let col_name_w = (available_w * 0.22).max(150.0);
    let col_company_w = (available_w * 0.16).max(120.0);
    let col_title_w = (available_w * 0.14).max(100.0);
    let col_email_w = (available_w * 0.20).max(140.0);
    let col_phone_w = (available_w * 0.14).max(110.0);
    let col_categories_w = (available_w - col_name_w - col_company_w - col_title_w - col_email_w - col_phone_w).max(100.0);

    let height = (rows.len() as f64 * row_height).max(40.0);
    PreparedContactTableLayout {
        rows,
        width: available_w,
        height,
        row_height,
        col_name_w,
        col_company_w,
        col_title_w,
        col_email_w,
        col_phone_w,
        col_categories_w,
    }
}

pub(crate) fn hit_test_contact_table_header(
    layout: &PreparedContactTableLayout,
    x: f64,
) -> Option<ContactTableColumn> {
    if x < 0.0 || x > layout.width {
        return None;
    }
    let mut cur = 0.0;
    cur += layout.col_name_w;
    if x <= cur {
        return Some(ContactTableColumn::Name);
    }
    cur += layout.col_company_w;
    if x <= cur {
        return Some(ContactTableColumn::Company);
    }
    cur += layout.col_title_w;
    if x <= cur {
        return Some(ContactTableColumn::Title);
    }
    cur += layout.col_email_w;
    if x <= cur {
        return Some(ContactTableColumn::Email);
    }
    cur += layout.col_phone_w;
    if x <= cur {
        return Some(ContactTableColumn::Phone);
    }
    Some(ContactTableColumn::Categories)
}

pub(crate) fn hit_test_contact_table(
    layout: &PreparedContactTableLayout,
    y: f64,
) -> Option<usize> {
    if y < 0.0 || y > layout.height {
        return None;
    }
    let idx = (y / layout.row_height) as usize;
    if idx < layout.rows.len() {
        Some(idx)
    } else {
        None
    }
}

pub(crate) fn draw_contact_table_header(
    cr: &gtk::cairo::Context,
    layout: &PreparedContactTableLayout,
    sort_col: ContactTableColumn,
    sort_desc: bool,
    hovered_col: Option<ContactTableColumn>,
    is_dark: bool,
) {
    let w = layout.width;
    let h = 28.0;

    if is_dark {
        cr.set_source_rgb(0.14, 0.14, 0.14);
    } else {
        cr.set_source_rgb(0.96, 0.96, 0.96);
    }
    let _ = cr.paint();

    let columns = [
        (ContactTableColumn::Name, "Name", layout.col_name_w),
        (ContactTableColumn::Company, "Organisation", layout.col_company_w),
        (ContactTableColumn::Title, "Position", layout.col_title_w),
        (ContactTableColumn::Email, "E-Mail", layout.col_email_w),
        (ContactTableColumn::Phone, "Telefon", layout.col_phone_w),
        (ContactTableColumn::Categories, "Kategorien", layout.col_categories_w),
    ];

    let (text_r, text_g, text_b) = if is_dark { (0.80, 0.80, 0.80) } else { (0.35, 0.35, 0.35) };
    let (div_r, div_g, div_b) = if is_dark { (1.0, 1.0, 1.0) } else { (0.0, 0.0, 0.0) };

    let mut col_x = 0.0;
    for (col, label, col_w) in columns {
        if Some(col) == hovered_col {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
            }
            cr.rectangle(col_x, 0.0, col_w, h);
            let _ = cr.fill();
        }

        let _ = cr.save();
        cr.rectangle(col_x + 8.0, 0.0, (col_w - 16.0).max(10.0), h);
        let _ = cr.clip();

        cr.set_source_rgb(text_r, text_g, text_b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
        cr.set_font_size(11.0);
        cr.move_to(col_x + 8.0, 18.0);
        let _ = cr.show_text(label);

        if col == sort_col {
            let arrow = if sort_desc { " ▼" } else { " ▲" };
            let _ = cr.show_text(arrow);
        }
        let _ = cr.restore();

        col_x += col_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.12);
        cr.set_line_width(1.0);
        cr.move_to(col_x - 0.5, 4.0);
        cr.line_to(col_x - 0.5, h - 4.0);
        let _ = cr.stroke();
    }

    cr.set_source_rgba(div_r, div_g, div_b, 0.15);
    cr.set_line_width(1.0);
    cr.move_to(0.0, h - 0.5);
    cr.line_to(w, h - 0.5);
    let _ = cr.stroke();
}

pub(crate) fn draw_contact_table_canvas(
    cr: &gtk::cairo::Context,
    layout: &PreparedContactTableLayout,
    width: f64,
    height: f64,
    hovered_row: Option<usize>,
    is_dark: bool,
) {
    let (clip_y1, clip_y2) = match cr.clip_extents() {
        Ok((_, y1, _, y2)) => (y1, y2),
        Err(_) => (0.0, height),
    };

    let start_row = (clip_y1 / layout.row_height).floor().max(0.0) as usize;
    let end_row = ((clip_y2 / layout.row_height).ceil() as usize).min(layout.rows.len());

    if is_dark {
        cr.set_source_rgb(0.12, 0.12, 0.12);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    let _ = cr.paint();

    let (r, g, b) = if is_dark { (0.92, 0.92, 0.92) } else { (0.12, 0.12, 0.12) };
    let (dim_r, dim_g, dim_b) = if is_dark { (0.60, 0.60, 0.60) } else { (0.50, 0.50, 0.50) };
    let (div_r, div_g, div_b) = if is_dark { (1.0, 1.0, 1.0) } else { (0.0, 0.0, 0.0) };

    for row_idx in start_row..end_row {
        let Some(row) = layout.rows.get(row_idx) else { break; };
        let y = row.y;
        let h = row.height;

        if row.selected {
            if is_dark {
                cr.set_source_rgba(0.20, 0.38, 0.65, 0.55);
            } else {
                cr.set_source_rgba(0.22, 0.50, 0.88, 0.22);
            }
            cr.rectangle(0.0, y, width, h);
            let _ = cr.fill();

            cr.set_source_rgba(0.21, 0.51, 0.96, 1.0);
            cr.rectangle(0.0, y, 3.0, h);
            let _ = cr.fill();
        } else if Some(row_idx) == hovered_row {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            cr.rectangle(0.0, y, width, h);
            let _ = cr.fill();
        }

        cr.set_source_rgba(div_r, div_g, div_b, 0.06);
        cr.set_line_width(1.0);
        cr.move_to(0.0, y + h - 0.5);
        cr.line_to(width, y + h - 0.5);
        let _ = cr.stroke();

        let mut col_x = 0.0;

        // 1. Name Column (with avatar initials circle)
        let name_w = layout.col_name_w;
        let _ = cr.save();
        cr.rectangle(col_x + 4.0, y, (name_w - 8.0).max(10.0), h);
        let _ = cr.clip();

        let avatar_cx = col_x + 16.0;
        let avatar_cy = y + h / 2.0;
        cr.set_source_rgba(0.21, 0.51, 0.96, 0.25);
        cr.arc(avatar_cx, avatar_cy, 9.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();

        let initials: String = row
            .name
            .split_whitespace()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect();
        cr.set_source_rgb(0.21, 0.51, 0.96);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
        cr.set_font_size(9.0);
        cr.move_to(avatar_cx - 5.0, avatar_cy + 3.5);
        let _ = cr.show_text(&initials);

        cr.set_source_rgb(r, g, b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
        cr.set_font_size(12.0);
        cr.move_to(col_x + 32.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&row.name);
        let _ = cr.restore();
        col_x += name_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.08);
        cr.move_to(col_x - 0.5, y + 3.0);
        cr.line_to(col_x - 0.5, y + h - 3.0);
        let _ = cr.stroke();

        // 2. Company Column
        let comp_w = layout.col_company_w;
        let _ = cr.save();
        cr.rectangle(col_x + 6.0, y, (comp_w - 12.0).max(10.0), h);
        let _ = cr.clip();
        cr.set_source_rgb(dim_r, dim_g, dim_b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        cr.move_to(col_x + 6.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&row.company);
        let _ = cr.restore();
        col_x += comp_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.08);
        cr.move_to(col_x - 0.5, y + 3.0);
        cr.line_to(col_x - 0.5, y + h - 3.0);
        let _ = cr.stroke();

        // 3. Title Column
        let title_w = layout.col_title_w;
        let _ = cr.save();
        cr.rectangle(col_x + 6.0, y, (title_w - 12.0).max(10.0), h);
        let _ = cr.clip();
        cr.set_source_rgb(dim_r, dim_g, dim_b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        cr.move_to(col_x + 6.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&row.title);
        let _ = cr.restore();
        col_x += title_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.08);
        cr.move_to(col_x - 0.5, y + 3.0);
        cr.line_to(col_x - 0.5, y + h - 3.0);
        let _ = cr.stroke();

        // 4. Email Column
        let email_w = layout.col_email_w;
        let _ = cr.save();
        cr.rectangle(col_x + 6.0, y, (email_w - 12.0).max(10.0), h);
        let _ = cr.clip();
        if is_dark {
            cr.set_source_rgb(0.45, 0.70, 1.0);
        } else {
            cr.set_source_rgb(0.15, 0.45, 0.85);
        }
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        cr.move_to(col_x + 6.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&row.email);
        let _ = cr.restore();
        col_x += email_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.08);
        cr.move_to(col_x - 0.5, y + 3.0);
        cr.line_to(col_x - 0.5, y + h - 3.0);
        let _ = cr.stroke();

        // 5. Phone Column
        let phone_w = layout.col_phone_w;
        let _ = cr.save();
        cr.rectangle(col_x + 6.0, y, (phone_w - 12.0).max(10.0), h);
        let _ = cr.clip();
        cr.set_source_rgb(dim_r, dim_g, dim_b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        cr.move_to(col_x + 6.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&row.phone);
        let _ = cr.restore();
        col_x += phone_w;

        cr.set_source_rgba(div_r, div_g, div_b, 0.08);
        cr.move_to(col_x - 0.5, y + 3.0);
        cr.line_to(col_x - 0.5, y + h - 3.0);
        let _ = cr.stroke();

        // 6. Categories Column
        let cat_w = layout.col_categories_w;
        let _ = cr.save();
        cr.rectangle(col_x + 6.0, y, (cat_w - 12.0).max(10.0), h);
        let _ = cr.clip();
        let cat_str = row.categories.join(", ");
        cr.set_source_rgb(dim_r, dim_g, dim_b);
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        cr.move_to(col_x + 6.0, y + h / 2.0 + 4.0);
        let _ = cr.show_text(&cat_str);
        let _ = cr.restore();
    }
}

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(Clone)]
    pub struct ReloadAddressBooksCallback(pub Rc<dyn Fn()>);

    impl std::fmt::Debug for ReloadAddressBooksCallback {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ReloadAddressBooksCallback")
        }
    }

    #[derive(Clone)]
    pub struct ReloadContactsCallback(pub Rc<dyn Fn()>);

    impl std::fmt::Debug for ReloadContactsCallback {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ReloadContactsCallback")
        }
    }

    #[derive(Debug, Default)]
    pub struct ContactsPage {
        pub ebook_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub contact_search_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub contact_search_field: Cell<ContactSearchField>,
        pub contact_search_status: RefCell<Option<gtk::Label>>,
        pub contact_search_spinner: RefCell<Option<gtk::Spinner>>,
        pub contact_search_bar: RefCell<Option<gtk::SearchBar>>,
        pub disposed: Cell<bool>,
        pub selected_identity: RefCell<Option<ContactIdentity>>,
        pub new_contact_button: RefCell<Option<gtk::Button>>,
        pub new_list_button: RefCell<Option<gtk::Button>>,
        pub drawer_total: RefCell<Option<gtk::Label>>,
        pub contact_list: RefCell<Option<gtk::ListBox>>,
        pub contact_table_header: RefCell<Option<gtk::DrawingArea>>,
        pub contact_table_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub contact_table_layout: RefCell<Option<PreparedContactTableLayout>>,
        pub contact_table_hovered_row: Cell<Option<usize>>,
        pub contact_table_hovered_header: Cell<Option<ContactTableColumn>>,
        pub contact_table_anchor_row: Cell<Option<usize>>,
        pub contact_sort_col: Cell<ContactTableColumn>,
        pub contact_sort_desc: Cell<bool>,
        pub stored_contacts: RefCell<Option<Rc<RefCell<Vec<EBookContactRecord>>>>>,
        pub search_state: RefCell<Option<Rc<ContactPageSearchState>>>,
        pub contact_cards_flow: RefCell<Option<gtk::FlowBox>>,
        pub cards_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub view_stack: RefCell<Option<gtk::Stack>>,
        pub alphabet_box: RefCell<Option<gtk::ListBox>>,
        pub list_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub outer_paned: RefCell<Option<gtk::Paned>>,
        pub reload_fn: RefCell<Option<ReloadContactsCallback>>,
        pub contact_preview_paned: RefCell<Option<gtk::Paned>>,
        pub contact_preview_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub contacts_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub collapsed_addressbook_groups: RefCell<HashSet<String>>,
        pub is_syncing_selection: Cell<bool>,
        pub toolbar: RefCell<Option<gtk::Widget>>,
        pub macos_banner: RefCell<Option<adw::Banner>>,
        pub reload_address_books_fn: RefCell<Option<ReloadAddressBooksCallback>>,
        pub is_mobile: Cell<bool>,
        pub is_tablet: Cell<bool>,
        pub is_detail_shown: Cell<bool>,
        pub edit_button: RefCell<Option<gtk::Button>>,
        pub email_button: RefCell<Option<gtk::Button>>,
        pub delete_button: RefCell<Option<gtk::Button>>,
        pub print_button: RefCell<Option<gtk::Button>>,
        pub print_list_button: RefCell<Option<gtk::Button>>,
        pub export_button: RefCell<Option<gtk::Button>>,
        pub merge_button: RefCell<Option<gtk::Button>>,
        pub copy_to_book_button: RefCell<Option<gtk::Button>>,
        pub move_to_book_button: RefCell<Option<gtk::Button>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContactsPage {
        const NAME: &'static str = "EvolutionContactsPage";
        type Type = super::ContactsPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for ContactsPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_vexpand(true);
            self.obj().set_hexpand(true);
            self.obj().build_ui();
        }

        fn dispose(&self) {
            self.disposed.set(true);
            if let Some(cancel) = self.ebook_monitor_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(cancel) = self.contact_search_cancel.borrow_mut().take() {
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ContactsPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct ContactsPage(ObjectSubclass<imp::ContactsPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub(crate) fn current_contact_selected_identities(
    contact_list: &gtk::ListBox,
) -> HashSet<ContactIdentity> {
    contact_list
        .selected_rows()
        .iter()
        .filter_map(row_contact_identity)
        .collect()
}

impl ContactsPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub(crate) fn queue_contacts_table_draw(&self) {
        if let Some(canvas) = self.imp().contact_table_canvas.borrow().as_ref() {
            canvas.queue_draw();
        }
        if let Some(header) = self.imp().contact_table_header.borrow().as_ref() {
            header.queue_draw();
        }
    }

    pub(crate) fn update_contacts_table_layout(
        &self,
        stored_contacts: &[EBookContactRecord],
        visible_filter: Option<&HashSet<ContactIdentity>>,
        selected_identities: &HashSet<ContactIdentity>,
    ) {
        let sort_col = self.imp().contact_sort_col.get();
        let sort_desc = self.imp().contact_sort_desc.get();
        let w = self
            .imp()
            .contact_table_canvas
            .borrow()
            .as_ref()
            .map(|c| c.width() as f64)
            .unwrap_or(800.0)
            .max(800.0);
        let layout = prepare_contact_table_layout(
            stored_contacts,
            visible_filter,
            selected_identities,
            sort_col,
            sort_desc,
            w,
        );
        if let Some(canvas) = self.imp().contact_table_canvas.borrow().as_ref() {
            canvas.set_content_height(layout.height as i32);
            canvas.set_content_width(layout.width as i32);
        }
        if let Some(header) = self.imp().contact_table_header.borrow().as_ref() {
            header.set_content_width(layout.width as i32);
        }
        self.imp().contact_table_layout.replace(Some(layout));
        self.queue_contacts_table_draw();
    }

    pub(crate) fn refresh_table_canvas(&self) {
        let stored_opt = self.imp().stored_contacts.borrow().clone();
        let list_opt = self.imp().contact_list.borrow().clone();
        let search_opt = self.imp().search_state.borrow().clone();
        if let (Some(stored), Some(list), Some(search)) = (stored_opt, list_opt, search_opt) {
            let vis = search.visible_identities.borrow();
            let visible_filter = if vis.is_empty() && search.active_query().clauses.is_empty() {
                None
            } else {
                Some(&*vis)
            };
            let selected = current_contact_selected_identities(&list);
            self.update_contacts_table_layout(&stored.borrow(), visible_filter, &selected);
        }
    }

    pub fn reload_contacts(&self) {
        if let Some(reload) = self.imp().reload_fn.borrow().as_ref() {
            (reload.0)();
        }
    }

    /// Trigger the same contact editor as the page-local toolbar button.  The
    /// main Evolution window uses this to keep menu, header, and page actions
    /// on one persistent list-refresh path.
    pub fn present_new_contact(&self) {
        if let Some(button) = self.imp().new_contact_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub fn present_new_contact_list(&self) {
        if let Some(button) = self.imp().new_list_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub fn update_drawer_total(&self, total: usize) {
        if let Some(lbl) = self.imp().drawer_total.borrow().as_ref() {
            if total > 0 {
                lbl.set_label(&total.to_string());
                lbl.set_tooltip_text(Some(&format!("{total} Kontakte insgesamt")));
            } else {
                lbl.set_label("");
                lbl.set_tooltip_text(None);
            }
        }
    }

    pub fn reload_address_books(&self) {
        if let Some(reload_fn) = self.imp().reload_address_books_fn.borrow().as_ref() {
            (reload_fn.0)();
        }
    }

    #[cfg(all(target_os = "macos", macos_bridge))]
    pub fn update_macos_permission_banner(&self) {
        let Some(banner) = self.imp().macos_banner.borrow().as_ref().cloned() else {
            return;
        };
        let srv = crate::services::MacosIntegrationService::new();
        let status = srv.check_permission(crate::services::MacosPermissionKind::Contacts);
        match status {
            crate::services::MacosPermissionStatus::NotDetermined => {
                banner.set_title("Apple Kontakte verbinden: Möchten Sie Ihre macOS Kontakte in Evolution anzeigen?");
                banner.set_button_label(Some("Apple Kontakte verbinden"));
                banner.set_revealed(true);
            }
            crate::services::MacosPermissionStatus::Denied => {
                banner.set_title("Zugriff auf Apple Kontakte nicht erlaubt. In den macOS Systemeinstellungen aktivieren.");
                banner.set_button_label(Some("Systemeinstellungen öffnen"));
                banner.set_revealed(true);
            }
            _ => {
                banner.set_revealed(false);
            }
        }
    }

    pub fn outer_paned(&self) -> Option<gtk::Paned> {
        self.imp().outer_paned.borrow().clone()
    }

    pub fn contact_preview_paned(&self) -> Option<gtk::Paned> {
        self.imp().contact_preview_paned.borrow().clone()
    }

    pub fn is_preview_visible(&self) -> bool {
        if let Some(preview_paned) = self.imp().contact_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                return detail_box.is_visible();
            }
        }
        AppSettings::try_new()
            .map(|s| s.contact_preview_visible())
            .unwrap_or(true)
    }

    pub fn set_preview_visible(&self, visible: bool) {
        if let Some(preview_paned) = self.imp().contact_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                detail_box.set_visible(visible);
            }
        }
        if let Some(toggle) = self.imp().contact_preview_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_contact_preview_visible(visible);
        }
    }

    pub fn is_sidebar_visible(&self) -> bool {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if let Some(start) = paned.start_child() {
                return start.is_visible();
            }
        }
        true
    }

    pub fn set_sidebar_visible(&self, visible: bool) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if let Some(start) = paned.start_child() {
                start.set_visible(visible);
            }
            if visible {
                let saved_pos = AppSettings::try_new()
                    .map(|s| s.mail_sidebar_position())
                    .unwrap_or(260)
                    .clamp(240, 1500);
                if paned.position() < 180 {
                    paned.set_position(saved_pos);
                }
            }
        }
        if let Some(toggle) = self.imp().contacts_sidebar_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
    }

    pub(crate) fn set_screen_mode(&self, is_mobile: bool, is_tablet: bool) {
        let imp = self.imp();
        imp.is_mobile.set(is_mobile);
        imp.is_tablet.set(is_tablet);
        self.sync_contacts_layout_visibility();
        self.sync_toolbar_button_labels();
    }

    pub(crate) fn apply_toolbar_button_modes(&self, is_compact: bool) {
        let imp = self.imp();
        let buttons: [(&RefCell<Option<gtk::Button>>, &str, &str); 9] = [
            (&imp.edit_button, "Bearbeiten", "document-edit-symbolic"),
            (&imp.email_button, "E-Mail", "mail-message-new-symbolic"),
            (&imp.delete_button, "Löschen", "user-trash-symbolic"),
            (&imp.print_button, "Drucken", "document-print-symbolic"),
            (&imp.print_list_button, "Liste drucken", "document-print-preview-symbolic"),
            (&imp.export_button, "Exportieren", "document-save-symbolic"),
            (&imp.merge_button, "Zusammenführen", "object-select-symbolic"),
            (&imp.copy_to_book_button, "Kopieren…", "edit-copy-symbolic"),
            (&imp.move_to_book_button, "Verschieben…", "folder-move-symbolic"),
        ];

        for (cell, text, icon) in buttons {
            if let Some(btn) = cell.borrow().as_ref() {
                if is_compact {
                    btn.set_child(Some(&gtk::Image::from_icon_name(icon)));
                } else {
                    let label = gtk::Label::builder()
                        .label(text)
                        .valign(gtk::Align::Center)
                        .build();
                    btn.set_child(Some(&label));
                }
            }
        }
    }

    pub(crate) fn sync_toolbar_button_labels(&self) {
        let is_compact = self.imp().is_mobile.get() || (self.width() > 0 && self.width() < 820);
        self.apply_toolbar_button_modes(is_compact);
    }

    pub(crate) fn can_navigate_back(&self) -> bool {
        let imp = self.imp();
        imp.is_mobile.get() && imp.is_detail_shown.get()
    }

    pub(crate) fn navigate_back(&self) -> bool {
        let imp = self.imp();
        if imp.is_mobile.get() && imp.is_detail_shown.get() {
            imp.is_detail_shown.set(false);
            self.sync_contacts_layout_visibility();
            if let Some(root) = self.root().and_downcast::<crate::window::EvolutionWindow>() {
                root.sync_back_button();
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn sync_contacts_layout_visibility(&self) {
        let imp = self.imp();
        let is_mobile = imp.is_mobile.get();
        let is_tablet = imp.is_tablet.get();
        let Some(outer_paned) = imp.outer_paned.borrow().clone() else { return; };
        let Some(preview_paned) = imp.contact_preview_paned.borrow().clone() else { return; };

        let sidebar = outer_paned.start_child();
        let list_container = preview_paned.start_child();
        let detail_box = preview_paned.end_child();

        if is_mobile {
            if let Some(sb) = sidebar.as_ref() {
                sb.set_visible(false);
            }
            if let Some(lc) = list_container.as_ref() {
                lc.set_size_request(0, -1);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_size_request(0, -1);
            }
            if imp.is_detail_shown.get() {
                if let Some(lc) = list_container.as_ref() {
                    lc.set_visible(false);
                }
                if let Some(db) = detail_box.as_ref() {
                    db.set_visible(true);
                }
            } else {
                if let Some(lc) = list_container.as_ref() {
                    lc.set_visible(true);
                }
                if let Some(db) = detail_box.as_ref() {
                    db.set_visible(false);
                }
            }
        } else if is_tablet {
            if let Some(lc) = list_container.as_ref() {
                lc.set_size_request(240, -1);
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_size_request(280, -1);
                db.set_visible(true);
            }
        } else {
            if let Some(lc) = list_container.as_ref() {
                lc.set_size_request(280, -1);
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_size_request(340, -1);
                db.set_visible(true);
            }
        }

        if let Some(toolbar) = imp.toolbar.borrow().as_ref() {
            if is_mobile {
                toolbar.set_visible(false);
            } else {
                let show_tb = AppSettings::try_new()
                    .map(|s| s.contact_show_toolbar())
                    .unwrap_or(true);
                toolbar.set_visible(show_tb);
            }
        }
    }

    pub fn is_toolbar_visible(&self) -> bool {
        self.imp()
            .toolbar
            .borrow()
            .as_ref()
            .map(|t| t.is_visible())
            .unwrap_or(true)
    }

    pub fn set_toolbar_visible(&self, visible: bool) {
        if let Some(toolbar) = self.imp().toolbar.borrow().as_ref() {
            toolbar.set_visible(visible);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_contact_show_toolbar(visible);
        }
    }

    pub fn contact_preview_toggle(&self) -> Option<gtk::ToggleButton> {
        self.imp().contact_preview_toggle.borrow().clone()
    }

    pub fn set_sidebar_position(&self, pos: i32) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if (paned.position() - pos).abs() > 1 {
                paned.set_position(pos);
            }
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(bar) = self.imp().contact_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn is_search_active(&self) -> bool {
        self.imp()
            .contact_search_bar
            .borrow()
            .as_ref()
            .map(|b| b.is_search_mode())
            .unwrap_or(false)
    }

    pub(crate) fn close_search(&self) {
        if let Some(bar) = self.imp().contact_search_bar.borrow().as_ref() {
            bar.set_search_mode(false);
        }
    }

    pub(crate) fn search_bar(&self) -> Option<gtk::SearchBar> {
        self.imp().contact_search_bar.borrow().clone()
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(bar) = self.imp().contact_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        // Global search has an explicit, typed mapping in the service layer:
        // Any Field contains.  Reset the page-local quick mode before setting
        // the entry so a previous Name/Phone quick mode cannot reinterpret a
        // cross-component result.
        self.imp()
            .contact_search_field
            .set(ContactSearchField::AnyField);
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn select_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let Some(contact_list) = self.imp().contact_list.borrow().as_ref().cloned() else {
            return false;
        };
        select_contact_row(&contact_list, source_uid, uid)
    }

    fn select_sidebar_source_row(list: &gtk::ListBox, target_index: usize) {
        let mut child = list.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                let idx = unsafe { row.data::<usize>("source-index").map(|v| *v.as_ref()) };
                if idx == Some(target_index) {
                    list.select_row(Some(row));
                    return;
                }
            }
            child = widget.next_sibling();
        }
    }

    fn build_ui(&self) {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.imp().outer_paned.replace(Some(paned.clone()));

        // ── Contact List (Sidebar) ──────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_start(0)
            .margin_end(0)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["mail-sidebar"])
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Kontakte durchsuchen…")
            .build();
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let quick_mode = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Alle Felder",
                "Name",
                "E-Mail",
                "Telefon",
                "Kategorie",
            ])),
            None::<&gtk::Expression>,
        );
        quick_mode.set_tooltip_text(Some("Schnellsuchfeld auswählen"));

        let advanced_search_button = gtk::Button::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Erweiterte Suche…")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();

        let book_model = gtk::StringList::new(&["Alle Kontakte"]);
        let book_selector = gtk::DropDown::new(Some(book_model.clone()), None::<&gtk::Expression>);
        book_selector.set_tooltip_text(Some(
            "Die Auswahl wird als Standardadressbuch für den nächsten Start gespeichert",
        ));
        book_selector.set_margin_start(8);
        book_selector.set_margin_end(8);
        book_selector.set_margin_bottom(8);
        // The visible source list below is the Evolution-like navigation.
        // Keep the model-backed dropdown only as the internal selection
        // coordinator so settings and keyboard-driven selection share one
        // source of truth without rendering the same list twice.
        book_selector.set_visible(false);
        sidebar_box.append(&book_selector);

        let book_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        sidebar_box.append(&book_source_list);
        let book_selector_for_sidebar = book_selector.clone();
        book_source_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                if let Some(target_index) = unsafe { row.data::<usize>("source-index").map(|v| *v.as_ref()) } {
                    book_selector_for_sidebar.set_selected(target_index as u32);
                }
            }
        });

        let category_model = gtk::StringList::new(&["Alle Kategorien"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Kontaktkategorie auswählen"));
        category_selector.set_margin_bottom(6);

        let selected_book = Rc::new(RefCell::new(String::new()));
        let book_source_uids = Rc::new(RefCell::new(vec![String::new()]));
        let selected_category = Rc::new(RefCell::new(String::new()));
        let duplicate_only = Rc::new(Cell::new(false));
        let unmatched_category = Rc::new(Cell::new(false));

        let duplicate_filter_row = gtk::CheckButton::builder()
            .label("Nur Dubletten")
            .tooltip_text("Gleiche E-Mail, Telefonnummer oder Name ohne Kontaktweg anzeigen")
            .halign(gtk::Align::Start)
            .margin_bottom(4)
            .build();

        let unmatched_category_row = gtk::CheckButton::builder()
            .label("Nur ohne Kategorie")
            .tooltip_text("Kontakte ohne jegliche Kategorie anzeigen")
            .halign(gtk::Align::Start)
            .margin_bottom(4)
            .build();

        let empty_state = gtk::Label::builder()
            .label("Noch kein synchronisiertes Adressbuch geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let search_spinner = gtk::Spinner::new();
        search_spinner.set_visible(false);
        search_spinner.set_halign(gtk::Align::Center);
        self.imp()
            .contact_search_status
            .replace(Some(empty_state.clone()));
        self.imp()
            .contact_search_spinner
            .replace(Some(search_spinner.clone()));

        let book_model_for_load = book_model.clone();
        let book_source_uids_for_load = Rc::clone(&book_source_uids);
        let book_selector_for_load = book_selector.clone();
        let book_source_list_for_load = book_source_list.clone();
        let empty_state_for_load = empty_state.clone();
        let page_weak_for_load = self.downgrade();
        let render_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        let reload_books: Rc<dyn Fn()> = {
            let book_model = book_model_for_load.clone();
            let book_source_uids = Rc::clone(&book_source_uids_for_load);
            let book_selector = book_selector_for_load.clone();
            let book_source_list = book_source_list_for_load.clone();
            let empty_state = empty_state_for_load.clone();
            let page_weak = page_weak_for_load.clone();
            let render_holder = Rc::clone(&render_holder);
            Rc::new(move || {
                let book_model = book_model.clone();
                let book_source_uids = Rc::clone(&book_source_uids);
                let book_selector = book_selector.clone();
                let book_source_list = book_source_list.clone();
                let empty_state = empty_state.clone();
                let page_weak = page_weak.clone();
                let render_holder = Rc::clone(&render_holder);

                let default_source_uid = AppSettings::try_new()
                    .map(|settings| settings.default_address_book_source_uid())
                    .unwrap_or_default();

                glib::spawn_future_local(async move {
                    let discovery =
                        eds_service::discover_sources(eds_service::EdsSourceKind::AddressBook).await;
                    let status = discovery.status_text();
                    let mut sources = discovery.sources;
                    sources.retain(|source| source.enabled);

                    // Ensure all configured accounts are present
                    for collection in eds_service::load_configured_account_collections() {
                        if !collection.contacts_enabled {
                            continue;
                        }
                        let account_name = collection.display_name.clone();
                        let already_represented = sources.iter().any(|s| {
                            s.parent_name.as_deref() == Some(&account_name)
                                || s.uid == collection.uid
                                || s.uid == format!("account-contacts-{}", collection.uid)
                        });
                        if !already_represented {
                            sources.push(eds_service::EdsSource {
                                uid: format!("account-contacts-{}", collection.uid),
                                display_name: "Kontakte".to_string(),
                                backend_name: collection.backend_name.clone(),
                                enabled: collection.enabled,
                                parent_name: Some(account_name),
                            });
                        }
                    }

                    sources.sort_by_cached_key(|source| {
                        (source.display_name.to_lowercase(), source.uid.clone())
                    });

                    let mut source_groups = std::collections::HashMap::new();
                    for source in &sources {
                        let group = source
                            .parent_name
                            .clone()
                            .unwrap_or_else(|| eds_service::resolve_source_parent_name(&source.uid));
                        source_groups.insert(source.uid.clone(), group);
                    }
                    source_groups.insert(
                        LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string(),
                        "Auf diesem Computer".to_string(),
                    );
                    if !default_source_uid.is_empty() {
                        source_groups
                            .entry(default_source_uid.clone())
                            .or_insert_with(|| eds_service::resolve_source_parent_name(&default_source_uid));
                    }

                    let mut names = vec!["Alle Kontakte".to_string()];
                    let mut uids = vec![String::new()];
                    for source in sources {
                        names.push(contact_source_label(&source.display_name, &source.uid));
                        uids.push(source.uid);
                    }
                    if !default_source_uid.trim().is_empty()
                        && !uids.iter().any(|uid| uid == &default_source_uid)
                    {
                        names.push("Nicht verfügbare Quelle".to_string());
                        uids.push(default_source_uid.clone());
                    }
                    if crate::services::contact_store_service::ContactStoreService::new().is_writable() {
                        names.push("Lokales Adressbuch".to_string());
                        uids.push(LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string());
                    }
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    book_model.splice(0, book_model.n_items(), &name_refs);

                    let previous_selected_uid = {
                        let prev_uids = book_source_uids.borrow();
                        let idx = book_selector.selected() as usize;
                        prev_uids.get(idx).cloned().unwrap_or_default()
                    };

                    book_source_uids.replace(uids.clone());

                    let mut groups_order: Vec<String> = Vec::new();
                    let mut group_indices: std::collections::HashMap<String, Vec<usize>> =
                        std::collections::HashMap::new();
                    for index in 1..names.len() {
                        let source_uid = &uids[index];
                        let group = source_groups
                            .get(source_uid)
                            .cloned()
                            .unwrap_or_else(|| "Auf diesem Computer".to_string());
                        if !groups_order.contains(&group) {
                            groups_order.push(group.clone());
                        }
                        group_indices.entry(group).or_default().push(index);
                    }
                    groups_order.sort_by(|a, b| {
                        if a == "Auf diesem Computer" {
                            std::cmp::Ordering::Greater
                        } else if b == "Auf diesem Computer" {
                            std::cmp::Ordering::Less
                        } else {
                            a.to_lowercase().cmp(&b.to_lowercase())
                        }
                    });

                    let render_sidebar: Rc<dyn Fn()> = {
                        let book_source_list = book_source_list.clone();
                        let names = names.clone();
                        let uids = uids.clone();
                        let groups_order = groups_order.clone();
                        let group_indices = group_indices.clone();
                        let book_selector = book_selector.clone();
                        let page_weak = page_weak.clone();
                        let render_holder_for_toggle = Rc::clone(&render_holder);
                        Rc::new(move || {
                            let Some(page) = page_weak.upgrade() else { return; };
                            while let Some(child) = book_source_list.first_child() {
                                book_source_list.remove(&child);
                            }
                            let default_book_uid = AppSettings::try_new()
                                .map(|s| s.default_address_book_source_uid())
                                .unwrap_or_default();

                            // 1. Root row: "Alle Kontakte"
                            let root_box = gtk::Box::builder()
                                .orientation(gtk::Orientation::Horizontal)
                                .spacing(6)
                                .margin_start(6)
                                .margin_end(6)
                                .valign(gtk::Align::Center)
                                .build();
                            let root_icon = gtk::Image::from_icon_name("view-list-symbolic");
                            root_icon.add_css_class("dim-label");
                            root_box.append(&root_icon);
                            let root_label = gtk::Label::builder()
                                .label("Alle Kontakte")
                                .halign(gtk::Align::Start)
                                .hexpand(true)
                                .ellipsize(pango::EllipsizeMode::End)
                                .build();
                            root_box.append(&root_label);

                            let root_row = gtk::ListBoxRow::builder()
                                .child(&root_box)
                                .css_classes(vec!["mail-sidebar-row"])
                                .build();
                            unsafe {
                                root_row.set_data("source-index", 0usize);
                            }
                            book_source_list.append(&root_row);

                            let collapsed_groups = page.imp().collapsed_addressbook_groups.borrow().clone();

                            for group_name in &groups_order {
                                let Some(indices) = group_indices.get(group_name) else { continue; };
                                let is_collapsed = collapsed_groups.contains(group_name);

                                let group_box = gtk::Box::builder()
                                    .orientation(gtk::Orientation::Horizontal)
                                    .spacing(6)
                                    .margin_start(0)
                                    .margin_end(6)
                                    .valign(gtk::Align::Center)
                                    .css_classes(vec![
                                        "mail-sidebar-row",
                                        "calendar-sidebar-row",
                                        "calendar-sidebar-group-row",
                                        "sidebar-account-row",
                                        "account-root-row",
                                    ])
                                    .build();

                                let arrow_icon = if is_collapsed {
                                    "pan-end-symbolic"
                                } else {
                                    "pan-down-symbolic"
                                };
                                let arrow = gtk::Image::builder()
                                    .icon_name(arrow_icon)
                                    .pixel_size(16)
                                    .valign(gtk::Align::Center)
                                    .build();
                                arrow.add_css_class("dim-label");
                                group_box.append(&arrow);

                                let group_label = gtk::Label::builder()
                                    .label(group_name)
                                    .halign(gtk::Align::Start)
                                    .hexpand(true)
                                    .ellipsize(pango::EllipsizeMode::End)
                                    .css_classes(vec!["account-heading"])
                                    .build();
                                group_box.append(&group_label);

                                let group_row = gtk::ListBoxRow::builder()
                                    .child(&group_box)
                                    .activatable(true)
                                    .selectable(false)
                                    .css_classes(vec![
                                        "calendar-sidebar-group-row",
                                        "account-root-row",
                                        "sidebar-account-row",
                                    ])
                                    .tooltip_text(if is_collapsed {
                                        "Gruppe aufklappen"
                                    } else {
                                        "Gruppe einklappen"
                                    })
                                    .build();

                                let page_weak_toggle = page.downgrade();
                                let gname = group_name.clone();
                                let group_gesture = gtk::GestureClick::new();
                                let render_holder_click = Rc::clone(&render_holder_for_toggle);
                                group_gesture.connect_released(move |_, _, _, _| {
                                    if let Some(p) = page_weak_toggle.upgrade() {
                                        let mut col = p.imp().collapsed_addressbook_groups.borrow_mut();
                                        if col.contains(&gname) {
                                            col.remove(&gname);
                                        } else {
                                            col.insert(gname.clone());
                                        }
                                        drop(col);
                                        if let Some(render_fn) = render_holder_click.borrow().as_ref() {
                                            render_fn();
                                        }
                                    }
                                });
                                group_row.add_controller(group_gesture);
                                book_source_list.append(&group_row);

                                if is_collapsed {
                                    continue;
                                }

                                for &index in indices {
                                    let name = &names[index];
                                    let source_uid = &uids[index];

                                    let child_box = gtk::Box::builder()
                                        .orientation(gtk::Orientation::Horizontal)
                                        .spacing(6)
                                        .margin_start(20)
                                        .margin_end(4)
                                        .valign(gtk::Align::Center)
                                        .build();

                                    let image = gtk::Image::from_icon_name("x-office-address-book-symbolic");
                                    image.add_css_class("dim-label");
                                    image.set_pixel_size(16);
                                    child_box.append(&image);

                                    let child_label = gtk::Label::builder()
                                        .label(name)
                                        .halign(gtk::Align::Start)
                                        .hexpand(true)
                                        .ellipsize(pango::EllipsizeMode::End)
                                        .build();
                                    child_box.append(&child_label);

                                    let is_primary = (!default_book_uid.is_empty() && default_book_uid == *source_uid)
                                        || (default_book_uid.is_empty() && *source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID);
                                    if is_primary {
                                        let prim = crate::models::source_selector::create_primary_indicator();
                                        prim.set_pixel_size(14);
                                        child_box.append(&prim);
                                    }

                                    let book_selector = book_selector.clone();
                                    let book_source_list_menu = book_source_list.clone();
                                    let target_index = index as u32;
                                    let source_uid_for_menu = source_uid.clone();
                                    let name_for_menu = name.clone();
                                    let child_label_for_dialog = child_label.clone();
                                    let child_box_weak = child_box.downgrade();
                                    let uids_for_groups = uids.clone();
                                    let names_for_groups = names.clone();
                                    let menu_btn = crate::models::source_selector::create_source_row_menu_full(
                                        is_primary,
                                        "Adressbuch",
                                        {
                                            let book_selector = book_selector.clone();
                                            let book_source_list = book_source_list_menu.clone();
                                            move || {
                                                book_selector.set_selected(target_index);
                                                ContactsPage::select_sidebar_source_row(&book_source_list, target_index as usize);
                                            }
                                        },
                                        {
                                            let book_selector = book_selector.clone();
                                            let book_source_list = book_source_list_menu.clone();
                                            move || {
                                                book_selector.set_selected(0);
                                                ContactsPage::select_sidebar_source_row(&book_source_list, 0);
                                            }
                                        },
                                        {
                                            let uid = source_uid.clone();
                                            move || {
                                                if let Some(settings) = AppSettings::try_new() {
                                                    settings.set_default_address_book_source_uid(&uid);
                                                }
                                            }
                                        },
                                        Some({
                                            let uid = source_uid_for_menu.clone();
                                            let name = name_for_menu.clone();
                                            let child_box_weak = child_box_weak.clone();
                                            let child_label = child_label_for_dialog.clone();
                                            move || {
                                                let root_window = child_box_weak
                                                    .upgrade()
                                                    .and_then(|r| r.root())
                                                    .and_downcast::<gtk::Window>();
                                                let dialog = crate::dialogs::SourcePropertiesDialog::new(
                                                    &uid,
                                                    &name,
                                                    "Adressbuch",
                                                    "#3584e4",
                                                    true,
                                                    is_primary,
                                                );
                                                if let Some(parent) = root_window.as_ref() {
                                                    dialog.set_transient_for(Some(parent));
                                                }
                                                let child_label_for_save = child_label.clone();
                                                dialog.set_on_save(move |update| {
                                                    child_label_for_save.set_label(&update.display_name);
                                                    if update.is_default {
                                                        if let Some(settings) = AppSettings::try_new() {
                                                            settings.set_default_address_book_source_uid(
                                                                &update.source_uid,
                                                            );
                                                        }
                                                    }
                                                });
                                                dialog.present();
                                            }
                                        }),
                                        Some({
                                            let child_box_weak = child_box_weak.clone();
                                            let uids = uids_for_groups.clone();
                                            let names = names_for_groups.clone();
                                            move || {
                                                let root_window = child_box_weak
                                                    .upgrade()
                                                    .and_then(|r| r.root())
                                                    .and_downcast::<gtk::Window>();
                                                let mut selector =
                                                    crate::models::source_selector::SourceSelector::new(
                                                        crate::models::source_selector::SourceDomain::Contacts,
                                                    );
                                                let items: Vec<
                                                    crate::models::source_selector::SourceSelectorItem,
                                                > = uids
                                                    .iter()
                                                    .skip(1)
                                                    .zip(names.iter().skip(1))
                                                    .map(|(uid, name)| {
                                                        crate::models::source_selector::SourceSelectorItem::new(
                                                            uid,
                                                            name,
                                                            "contacts",
                                                            true,
                                                            uid == LOCAL_ADDRESS_BOOK_SOURCE_UID,
                                                        )
                                                    })
                                                    .collect();
                                                selector.update_sources(items);
                                                if let Some(settings) = AppSettings::try_new() {
                                                    selector.load_from_settings(&settings);
                                                }
                                                let dialog = crate::dialogs::ManageGroupsDialog::new(
                                                    root_window.as_ref(),
                                                    "Adressbuch",
                                                    &selector,
                                                    move |updated_selector| {
                                                        if let Some(settings) = AppSettings::try_new() {
                                                            updated_selector.save_to_settings(&settings);
                                                        }
                                                    },
                                                );
                                                dialog.present();
                                            }
                                        }),
                                    );
                                    menu_btn.set_valign(gtk::Align::Center);
                                    menu_btn.set_size_request(18, 18);
                                    child_box.append(&menu_btn);

                                    let row = gtk::ListBoxRow::builder()
                                        .child(&child_box)
                                        .css_classes(vec!["mail-sidebar-row", "sidebar-child-row"])
                                        .build();
                                    let right_click = gtk::GestureClick::builder()
                                        .button(3)
                                        .build();
                                    let menu_btn_clone = menu_btn.clone();
                                    right_click.connect_pressed(move |_, _, _, _| {
                                        menu_btn_clone.popup();
                                    });
                                    row.add_controller(right_click);
                                    unsafe {
                                        row.set_data("source-index", index);
                                    }
                                    book_source_list.append(&row);
                                }
                            }

                            ContactsPage::select_sidebar_source_row(&book_source_list, book_selector.selected() as usize);
                        })
                    };

                    *render_holder.borrow_mut() = Some(Rc::clone(&render_sidebar));
                    render_sidebar();

                    let selected_index = if !previous_selected_uid.is_empty() {
                        uids.iter().position(|uid| uid == &previous_selected_uid).unwrap_or_else(|| {
                            uids.iter().position(|uid| uid == &default_source_uid).unwrap_or(0)
                        })
                    } else {
                        uids.iter().position(|uid| uid == &default_source_uid).unwrap_or(0)
                    } as u32;

                    book_selector.set_selected(selected_index);
                    ContactsPage::select_sidebar_source_row(&book_source_list, selected_index as usize);
                    empty_state.set_label(&format!("{status}; Kontakte werden geladen."));
                });
            })
        };

        self.imp()
            .reload_address_books_fn
            .replace(Some(imp::ReloadAddressBooksCallback(reload_books.clone())));
        reload_books();

        let contact_list = gtk::ListBox::builder()
            // Evolution's address-book actions operate on the whole visible
            // selection.  Keep the detail pane single-contact specific, but
            // let the list retain a 0/1/n selection for batch-safe actions.
            .selection_mode(gtk::SelectionMode::Multiple)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["boxed-list", "contact-list-view"])
            .build();
        contact_list.set_sort_func(|left, right| unsafe {
            let value = |row: &gtk::ListBoxRow, key: &str| {
                row.data::<String>(key)
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default()
            };
            value(left, "contact-sort-key")
                .cmp(&value(right, "contact-sort-key"))
                .then_with(|| value(left, "contact-email").cmp(&value(right, "contact-email")))
                .then_with(|| {
                    value(left, "contact-source-uid").cmp(&value(right, "contact-source-uid"))
                })
                .then_with(|| value(left, "contact-id").cmp(&value(right, "contact-id")))
                .into()
        });
        self.imp().contact_list.replace(Some(contact_list.clone()));

        let stored_contacts = Rc::new(RefCell::new(Vec::<EBookContactRecord>::new()));
        let selected_contact = Rc::new(RefCell::new(None::<EBookContactRecord>));
        let refresh_generation = Rc::new(Cell::new(0u64));
        let search_state = Rc::new(ContactPageSearchState::default());
        self.imp()
            .contact_search_field
            .set(ContactSearchField::AnyField);

        set_contact_filter(&contact_list, Rc::clone(&search_state));

        refresh_contacts(
            contact_list.clone(),
            Rc::clone(&stored_contacts),
            empty_state.clone(),
            Rc::clone(&selected_contact),
            category_model.clone(),
            category_selector.clone(),
            Rc::clone(&selected_category),
            Rc::clone(&refresh_generation),
            self.downgrade(),
            Rc::clone(&search_state),
        );

        let contact_list_for_reload = contact_list.clone();
        let stored_contacts_for_reload = Rc::clone(&stored_contacts);
        let empty_state_for_reload = empty_state.clone();
        let selected_contact_for_reload = Rc::clone(&selected_contact);
        let category_model_for_reload = category_model.clone();
        let category_selector_for_reload = category_selector.clone();
        let selected_category_for_reload = Rc::clone(&selected_category);
        let refresh_generation_for_reload = Rc::clone(&refresh_generation);
        let search_state_for_reload = Rc::clone(&search_state);
        let page_weak_for_reload = self.downgrade();
        self.imp()
            .reload_fn
            .replace(Some(imp::ReloadContactsCallback(Rc::new(move || {
                refresh_contacts(
                    contact_list_for_reload.clone(),
                    Rc::clone(&stored_contacts_for_reload),
                    empty_state_for_reload.clone(),
                    Rc::clone(&selected_contact_for_reload),
                    category_model_for_reload.clone(),
                    category_selector_for_reload.clone(),
                    Rc::clone(&selected_category_for_reload),
                    Rc::clone(&refresh_generation_for_reload),
                    page_weak_for_reload.clone(),
                    Rc::clone(&search_state_for_reload),
                );
            }))));

        let search_state_for_search = Rc::clone(&search_state);
        let stored_contacts_for_search = Rc::clone(&stored_contacts);
        let page_weak_for_search = self.downgrade();
        let selected_book_for_search = Rc::clone(&selected_book);
        let selected_category_for_search = Rc::clone(&selected_category);
        let duplicate_only_for_search = Rc::clone(&duplicate_only);
        let unmatched_category_for_search = Rc::clone(&unmatched_category);
        search_entry.connect_search_changed(glib::clone!(
            #[strong]
            search_state_for_search,
            #[strong]
            stored_contacts_for_search,
            #[strong]
            page_weak_for_search,
            move |entry| {
                let quick_field = page_weak_for_search
                    .upgrade()
                    .map(|page| page.imp().contact_search_field.get())
                    .unwrap_or_else(|| search_state_for_search.quick_field());
                search_state_for_search.set_quick_field(quick_field);
                let query =
                    quick_query(search_state_for_search.quick_field(), entry.text().as_str());
                let query = with_contact_outer_filters(
                    query,
                    &selected_book_for_search,
                    &selected_category_for_search,
                    unmatched_category_for_search.get(),
                    duplicate_only_for_search.get(),
                );
                start_contact_search(
                    page_weak_for_search.clone(),
                    query,
                    Rc::clone(&search_state_for_search),
                    Rc::clone(&stored_contacts_for_search),
                );
            }
        ));

        let search_entry_for_quick_mode = search_entry.clone();
        let search_state_for_quick_mode = Rc::clone(&search_state);
        let stored_contacts_for_quick_mode = Rc::clone(&stored_contacts);
        let page_weak_for_quick_mode = self.downgrade();
        let selected_book_for_quick_mode = Rc::clone(&selected_book);
        let selected_category_for_quick_mode = Rc::clone(&selected_category);
        let duplicate_only_for_quick_mode = Rc::clone(&duplicate_only);
        let unmatched_category_for_quick_mode = Rc::clone(&unmatched_category);
        quick_mode.connect_selected_notify(move |mode| {
            let field = quick_field_for_index(mode.selected());
            if let Some(page) = page_weak_for_quick_mode.upgrade() {
                page.imp().contact_search_field.set(field);
            }
            search_state_for_quick_mode.set_quick_field(field);
            let query = quick_query(field, search_entry_for_quick_mode.text().as_str());
            let query = with_contact_outer_filters(
                query,
                &selected_book_for_quick_mode,
                &selected_category_for_quick_mode,
                unmatched_category_for_quick_mode.get(),
                duplicate_only_for_quick_mode.get(),
            );
            start_contact_search(
                page_weak_for_quick_mode.clone(),
                query,
                Rc::clone(&search_state_for_quick_mode),
                Rc::clone(&stored_contacts_for_quick_mode),
            );
        });

        let selected_book_for_filter = Rc::clone(&selected_book);
        let selected_category_for_book = Rc::clone(&selected_category);
        let duplicate_only_for_book = Rc::clone(&duplicate_only);
        let unmatched_category_for_book = Rc::clone(&unmatched_category);
        let book_source_uids_for_filter = Rc::clone(&book_source_uids);
        let book_source_list_for_filter = book_source_list.clone();
        let search_state_for_book = Rc::clone(&search_state);
        let stored_contacts_for_book = Rc::clone(&stored_contacts);
        let page_weak_for_book = self.downgrade();
        book_selector.connect_selected_notify(move |selector| {
            let selected = book_source_uids_for_filter
                .borrow()
                .get(selector.selected() as usize)
                .cloned()
                .unwrap_or_default();
            *selected_book_for_filter.borrow_mut() = selected.clone();
            if let Some(settings) = AppSettings::try_new() {
                settings.set_default_address_book_source_uid(&selected);
            }
            ContactsPage::select_sidebar_source_row(&book_source_list_for_filter, selector.selected() as usize);
            let query = search_state_for_book.active_query_with_filters(current_outer_filters(
                &selected_book_for_filter,
                &selected_category_for_book,
                unmatched_category_for_book.get(),
                duplicate_only_for_book.get(),
            ));
            start_contact_search(
                page_weak_for_book.clone(),
                query,
                Rc::clone(&search_state_for_book),
                Rc::clone(&stored_contacts_for_book),
            );
        });

        if let Some(settings) = AppSettings::try_new() {
            let book_selector_for_settings = book_selector.clone();
            let book_source_uids_for_settings = Rc::clone(&book_source_uids);
            settings.connect_changed(Some("default-address-book-source"), move |settings, _| {
                let value = settings.string("default-address-book-source");
                let Some(selected) = book_source_uids_for_settings
                    .borrow()
                    .iter()
                    .position(|uid| uid == value.as_str())
                else {
                    return;
                };
                book_selector_for_settings.set_selected(selected as u32);
            });
        }

        let selected_book_for_duplicates = Rc::clone(&selected_book);
        let selected_category_for_duplicates = Rc::clone(&selected_category);
        let duplicate_only_for_duplicates = Rc::clone(&duplicate_only);
        let unmatched_category_for_duplicates = Rc::clone(&unmatched_category);
        let search_state_for_duplicates = Rc::clone(&search_state);
        let stored_contacts_for_duplicates = Rc::clone(&stored_contacts);
        let page_weak_for_duplicates = self.downgrade();
        duplicate_filter_row.connect_active_notify(move |row| {
            duplicate_only_for_duplicates.set(row.is_active());
            let query =
                search_state_for_duplicates.active_query_with_filters(current_outer_filters(
                    &selected_book_for_duplicates,
                    &selected_category_for_duplicates,
                    unmatched_category_for_duplicates.get(),
                    duplicate_only_for_duplicates.get(),
                ));
            start_contact_search(
                page_weak_for_duplicates.clone(),
                query,
                Rc::clone(&search_state_for_duplicates),
                Rc::clone(&stored_contacts_for_duplicates),
            );
        });

        let selected_book_for_unmatched = Rc::clone(&selected_book);
        let selected_category_for_unmatched = Rc::clone(&selected_category);
        let duplicate_only_for_unmatched = Rc::clone(&duplicate_only);
        let unmatched_category_for_filter = Rc::clone(&unmatched_category);
        let search_state_for_unmatched = Rc::clone(&search_state);
        let stored_contacts_for_unmatched = Rc::clone(&stored_contacts);
        let page_weak_for_unmatched = self.downgrade();
        unmatched_category_row.connect_active_notify(move |row| {
            unmatched_category_for_filter.set(row.is_active());
            let query =
                search_state_for_unmatched.active_query_with_filters(current_outer_filters(
                    &selected_book_for_unmatched,
                    &selected_category_for_unmatched,
                    unmatched_category_for_filter.get(),
                    duplicate_only_for_unmatched.get(),
                ));
            start_contact_search(
                page_weak_for_unmatched.clone(),
                query,
                Rc::clone(&search_state_for_unmatched),
                Rc::clone(&stored_contacts_for_unmatched),
            );
        });

        let selected_category_for_selector = Rc::clone(&selected_category);
        let selected_book_for_category = Rc::clone(&selected_book);
        let duplicate_only_for_category = Rc::clone(&duplicate_only);
        let unmatched_category_for_category = Rc::clone(&unmatched_category);
        let category_model_for_selector = category_model.clone();
        let search_state_for_category = Rc::clone(&search_state);
        let stored_contacts_for_category = Rc::clone(&stored_contacts);
        let page_weak_for_category = self.downgrade();
        category_selector.connect_selected_notify(move |selector| {
            let category = if selector.selected() == 0 {
                String::new()
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            };
            selected_category_for_selector.replace(category);
            let query = search_state_for_category.active_query_with_filters(current_outer_filters(
                &selected_book_for_category,
                &selected_category_for_selector,
                unmatched_category_for_category.get(),
                duplicate_only_for_category.get(),
            ));
            start_contact_search(
                page_weak_for_category.clone(),
                query,
                Rc::clone(&search_state_for_category),
                Rc::clone(&stored_contacts_for_category),
            );
        });

        let search_state_for_advanced = Rc::clone(&search_state);
        let stored_contacts_for_advanced = Rc::clone(&stored_contacts);
        let page_weak_for_advanced = self.downgrade();
        let selected_book_for_advanced = Rc::clone(&selected_book);
        let selected_category_for_advanced = Rc::clone(&selected_category);
        let unmatched_category_for_advanced = Rc::clone(&unmatched_category);
        let duplicate_only_for_advanced = Rc::clone(&duplicate_only);
        advanced_search_button.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let initial = crate::dialogs::contact_search_dialog::seed_query_for_reopen(
                    &search_state_for_advanced.active_query_with_filters(current_outer_filters(
                        &selected_book_for_advanced,
                        &selected_category_for_advanced,
                        unmatched_category_for_advanced.get(),
                        duplicate_only_for_advanced.get(),
                    )),
                );
                let state_for_apply = Rc::clone(&search_state_for_advanced);
                let contacts_for_apply = Rc::clone(&stored_contacts_for_advanced);
                let page_for_apply = page_weak_for_advanced.clone();
                let selected_book_for_apply = Rc::clone(&selected_book_for_advanced);
                let selected_category_for_apply = Rc::clone(&selected_category_for_advanced);
                let unmatched_category_for_apply = Rc::clone(&unmatched_category_for_advanced);
                let duplicate_only_for_apply = Rc::clone(&duplicate_only_for_advanced);
                let dialog =
                    crate::dialogs::ContactSearchDialog::with_query(initial, move |query| {
                        let query = query.with_filters(current_outer_filters(
                            &selected_book_for_apply,
                            &selected_category_for_apply,
                            unmatched_category_for_apply.get(),
                            duplicate_only_for_apply.get(),
                        ));
                        start_contact_search(
                            page_for_apply.clone(),
                            query,
                            Rc::clone(&state_for_apply),
                            Rc::clone(&contacts_for_apply),
                        );
                    });
                if let Some(window) = page.root().and_downcast::<gtk::Window>() {
                    dialog.set_transient_for(Some(&window));
                }
                dialog.present();
            }
        ));

        self.imp().stored_contacts.replace(Some(Rc::clone(&stored_contacts)));
        self.imp().search_state.replace(Some(Rc::clone(&search_state)));

        let contact_table_header = gtk::DrawingArea::builder()
            .content_height(28)
            .hexpand(true)
            .css_classes(vec!["contacts-table-header"])
            .build();
        set_accessible_label(&contact_table_header, "Kontakttabellen-Kopfzeile");

        let contact_table_canvas = gtk::DrawingArea::builder()
            .content_height(100)
            .hexpand(true)
            .focusable(true)
            .can_focus(true)
            .focus_on_click(true)
            .css_classes(vec!["contacts-table-canvas"])
            .build();
        set_accessible_label(&contact_table_canvas, "Kontaktliste Canvas");

        self.imp().contact_table_header.replace(Some(contact_table_header.clone()));
        self.imp().contact_table_canvas.replace(Some(contact_table_canvas.clone()));

        contact_list.set_visible(false);

        let contact_list_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        contact_list_box.append(&contact_table_canvas);
        contact_list_box.append(&contact_list);

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&contact_list_box)
            .vexpand(true)
            .hexpand(true)
            .margin_end(32)
            .build();
        self.imp().list_scroll.replace(Some(list_scroll.clone()));

        let header_for_hadj = contact_table_header.clone();
        list_scroll.hadjustment().connect_value_changed(move |_| {
            header_for_hadj.queue_draw();
        });

        // Header Draw Func
        {
            let page_weak = self.downgrade();
            let list_scroll_clone = list_scroll.clone();
            contact_table_header.set_draw_func(move |_, cr, _width, _height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let sort_col = page.imp().contact_sort_col.get();
                let sort_desc = page.imp().contact_sort_desc.get();
                let hovered_col = page.imp().contact_table_hovered_header.get();
                let is_dark = adw::StyleManager::default().is_dark();
                let scroll_x = list_scroll_clone.hadjustment().value();
                let _ = cr.save();
                cr.translate(-scroll_x, 0.0);
                draw_contact_table_header(cr, layout, sort_col, sort_desc, hovered_col, is_dark);
                let _ = cr.restore();
            });
        }

        // Header Click Gesture
        {
            let header_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let list_scroll_clone = list_scroll.clone();
            header_click.connect_pressed(move |gesture, _n_press, x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let scroll_x = list_scroll_clone.hadjustment().value();
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                if let Some(col) = hit_test_contact_table_header(layout, x + scroll_x) {
                    let cur_col = page.imp().contact_sort_col.get();
                    let cur_desc = page.imp().contact_sort_desc.get();
                    let new_desc = if cur_col == col { !cur_desc } else { false };
                    page.imp().contact_sort_col.set(col);
                    page.imp().contact_sort_desc.set(new_desc);
                    drop(layout_ref);
                    page.refresh_table_canvas();
                }
            });
            contact_table_header.add_controller(header_click);
        }

        // Header Motion Controller
        {
            let header_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let list_scroll_clone = list_scroll.clone();
            let header_clone = contact_table_header.clone();
            header_motion.connect_motion(move |_, x, _y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let scroll_x = list_scroll_clone.hadjustment().value();
                let layout_ref = page.imp().contact_table_layout.borrow();
                let col = layout_ref.as_ref().and_then(|l| hit_test_contact_table_header(l, x + scroll_x));
                if page.imp().contact_table_hovered_header.get() != col {
                    page.imp().contact_table_hovered_header.set(col);
                    header_clone.queue_draw();
                }
            });
            let page_weak_leave = self.downgrade();
            let header_clone_leave = contact_table_header.clone();
            header_motion.connect_leave(move |_| {
                let Some(page) = page_weak_leave.upgrade() else { return; };
                if page.imp().contact_table_hovered_header.get().is_some() {
                    page.imp().contact_table_hovered_header.set(None);
                    header_clone_leave.queue_draw();
                }
            });
            contact_table_header.add_controller(header_motion);
        }

        // Canvas Draw Func
        {
            let page_weak = self.downgrade();
            contact_table_canvas.set_draw_func(move |_, cr, width, height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let hovered = page.imp().contact_table_hovered_row.get();
                let is_dark = adw::StyleManager::default().is_dark();
                draw_contact_table_canvas(cr, layout, width as f64, height as f64, hovered, is_dark);
            });
            let canvas_for_theme = contact_table_canvas.clone();
            let header_for_theme = contact_table_header.clone();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                canvas_for_theme.queue_draw();
                header_for_theme.queue_draw();
            });
        }

        // Canvas Click Gesture
        {
            let canvas_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let contact_list_clone = contact_list.clone();
            let canvas_clone = contact_table_canvas.clone();
            canvas_click.connect_pressed(move |gesture, n_press, _x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let Some(row_idx) = hit_test_contact_table(layout, y) else { return; };
                let Some(row) = layout.rows.get(row_idx) else { return; };

                canvas_clone.grab_focus();

                if n_press >= 2 {
                    select_contact_row(&contact_list_clone, &row.identity.source_uid, &row.identity.uid);
                    if let Some(selected_row) = contact_list_clone.selected_row() {
                        selected_row.activate();
                    }
                    return;
                }

                let state = gesture.current_event_state();
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK)
                    || state.contains(gdk::ModifierType::META_MASK);

                if shift {
                    let anchor = page.imp().contact_table_anchor_row.get().unwrap_or(row_idx);
                    let start = anchor.min(row_idx);
                    let end = anchor.max(row_idx);
                    contact_list_clone.unselect_all();
                    for i in start..=end {
                        if let Some(r) = layout.rows.get(i) {
                            select_contact_row_ext(&contact_list_clone, &r.identity.source_uid, &r.identity.uid, false, false);
                        }
                    }
                } else if ctrl {
                    select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, false, true);
                    page.imp().contact_table_anchor_row.set(Some(row_idx));
                } else {
                    select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                    page.imp().contact_table_anchor_row.set(Some(row_idx));
                }
                if page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_contacts_layout_visibility();
                    if let Some(root) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                        root.sync_back_button();
                    }
                }
            });
            contact_table_canvas.add_controller(canvas_click);
        }

        // Canvas Motion Controller
        {
            let canvas_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let canvas_clone = contact_table_canvas.clone();
            canvas_motion.connect_motion(move |_, _x, y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let row_idx = layout_ref.as_ref().and_then(|l| hit_test_contact_table(l, y));
                if page.imp().contact_table_hovered_row.get() != row_idx {
                    page.imp().contact_table_hovered_row.set(row_idx);
                    canvas_clone.queue_draw();
                }
            });
            let page_weak_leave = self.downgrade();
            let canvas_clone_leave = contact_table_canvas.clone();
            canvas_motion.connect_leave(move |_| {
                let Some(page) = page_weak_leave.upgrade() else { return; };
                if page.imp().contact_table_hovered_row.get().is_some() {
                    page.imp().contact_table_hovered_row.set(None);
                    canvas_clone_leave.queue_draw();
                }
            });
            contact_table_canvas.add_controller(canvas_motion);
        }

        // Canvas Key Controller
        {
            let key_controller = gtk::EventControllerKey::new();
            let page_weak = self.downgrade();
            let contact_list_clone = contact_list.clone();
            let list_scroll_clone = list_scroll.clone();
            key_controller.connect_key_pressed(move |_, keyval, _keycode, state| {
                let Some(page) = page_weak.upgrade() else { return glib::Propagation::Proceed; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return glib::Propagation::Proceed; };
                if layout.rows.is_empty() {
                    return glib::Propagation::Proceed;
                }

                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let current_anchor = page.imp().contact_table_anchor_row.get().unwrap_or(0).min(layout.rows.len() - 1);

                let scroll_to_row = |idx: usize| {
                    if let Some(row) = layout.rows.get(idx) {
                        let vadj = list_scroll_clone.vadjustment();
                        if row.y < vadj.value() {
                            vadj.set_value(row.y);
                        } else if row.y + row.height > vadj.value() + vadj.page_size() {
                            vadj.set_value(row.y + row.height - vadj.page_size());
                        }
                    }
                };

                match keyval {
                    gdk::Key::Up => {
                        let next_idx = current_anchor.saturating_sub(1);
                        if shift {
                            let anchor = page.imp().contact_table_anchor_row.get().unwrap_or(0);
                            let start = anchor.min(next_idx);
                            let end = anchor.max(next_idx);
                            contact_list_clone.unselect_all();
                            for i in start..=end {
                                if let Some(r) = layout.rows.get(i) {
                                    select_contact_row_ext(&contact_list_clone, &r.identity.source_uid, &r.identity.uid, false, false);
                                }
                            }
                        } else if let Some(row) = layout.rows.get(next_idx) {
                            page.imp().contact_table_anchor_row.set(Some(next_idx));
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Down => {
                        let next_idx = (current_anchor + 1).min(layout.rows.len() - 1);
                        if shift {
                            let anchor = page.imp().contact_table_anchor_row.get().unwrap_or(0);
                            let start = anchor.min(next_idx);
                            let end = anchor.max(next_idx);
                            contact_list_clone.unselect_all();
                            for i in start..=end {
                                if let Some(r) = layout.rows.get(i) {
                                    select_contact_row_ext(&contact_list_clone, &r.identity.source_uid, &r.identity.uid, false, false);
                                }
                            }
                        } else if let Some(row) = layout.rows.get(next_idx) {
                            page.imp().contact_table_anchor_row.set(Some(next_idx));
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Page_Up => {
                        let next_idx = current_anchor.saturating_sub(10);
                        page.imp().contact_table_anchor_row.set(Some(next_idx));
                        if let Some(row) = layout.rows.get(next_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Page_Down => {
                        let next_idx = (current_anchor + 10).min(layout.rows.len() - 1);
                        page.imp().contact_table_anchor_row.set(Some(next_idx));
                        if let Some(row) = layout.rows.get(next_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Home => {
                        page.imp().contact_table_anchor_row.set(Some(0));
                        if let Some(row) = layout.rows.first() {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(0);
                        glib::Propagation::Stop
                    }
                    gdk::Key::End => {
                        let last_idx = layout.rows.len().saturating_sub(1);
                        page.imp().contact_table_anchor_row.set(Some(last_idx));
                        if let Some(row) = layout.rows.get(last_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                        }
                        scroll_to_row(last_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Return | gdk::Key::KP_Enter => {
                        if let Some(selected_row) = contact_list_clone.selected_row() {
                            selected_row.activate();
                        }
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            contact_table_canvas.add_controller(key_controller);
        }

        let contact_table_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        contact_table_box.append(&contact_table_header);
        contact_table_box.append(&list_scroll);

        let contact_cards_flow = gtk::FlowBox::builder()
            .orientation(gtk::Orientation::Horizontal)
            .valign(gtk::Align::Start)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .max_children_per_line(12)
            .min_children_per_line(1)
            .selection_mode(gtk::SelectionMode::Single)
            .homogeneous(true)
            .column_spacing(10)
            .row_spacing(10)
            .margin_start(10)
            .margin_end(10)
            .margin_top(10)
            .margin_bottom(10)
            .css_classes(vec!["contact-cards-flow"])
            .build();
        self.imp().contact_cards_flow.replace(Some(contact_cards_flow.clone()));

        contact_cards_flow.set_sort_func(|child_a, child_b| unsafe {
            let key_a = child_a
                .data::<String>("contact-sort-key")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default();
            let key_b = child_b
                .data::<String>("contact-sort-key")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default();
            key_a.cmp(&key_b).into()
        });

        let search_state_cards = Rc::clone(&search_state);
        contact_cards_flow.set_filter_func(move |child| unsafe {
            let source_uid = child
                .data::<String>("contact-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let uid = child
                .data::<String>("contact-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            search_state_cards
                .visible_identities
                .borrow()
                .contains(&ContactIdentity::new(source_uid, uid))
        });

        let contact_list_flow = contact_list.clone();
        let page_weak_for_flow = self.downgrade();
        contact_cards_flow.connect_selected_children_changed(move |flow| {
            let Some(page) = page_weak_for_flow.upgrade() else {
                return;
            };
            if page.imp().is_syncing_selection.get() {
                return;
            }
            let selected = flow.selected_children();
            if let Some(child) = selected.first() {
                unsafe {
                    if let (Some(source_uid), Some(uid)) = (
                        child.data::<String>("contact-source-uid"),
                        child.data::<String>("contact-id"),
                    ) {
                        select_contact_row(&contact_list_flow, source_uid.as_ref(), uid.as_ref());
                    }
                }
            } else {
                contact_list_flow.unselect_all();
            }
        });

        let cards_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&contact_cards_flow)
            .vexpand(true)
            .hexpand(true)
            .margin_end(32)
            .build();
        self.imp().cards_scroll.replace(Some(cards_scroll.clone()));

        let view_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .hexpand(true)
            .build();
        view_stack.add_named(&cards_scroll, Some("cards"));
        view_stack.add_named(&contact_table_box, Some("list"));
        view_stack.set_visible_child_name("cards");
        self.imp().view_stack.replace(Some(view_stack.clone()));

        let alphabet_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["contacts-alphabet-pane"])
            .halign(gtk::Align::End)
            .width_request(30)
            .build();
        self.imp().alphabet_box.replace(Some(alphabet_box.clone()));

        let alphabet_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&alphabet_box)
            .propagate_natural_width(false)
            .min_content_width(30)
            .max_content_width(30)
            .width_request(30)
            .halign(gtk::Align::End)
            .vexpand(true)
            .hexpand(false)
            .margin_end(0)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let list_with_alphabet = gtk::Overlay::builder()
            .child(&view_stack)
            .vexpand(true)
            .hexpand(true)
            .build();
        list_with_alphabet.add_overlay(&alphabet_scroll);
        list_with_alphabet.set_measure_overlay(&alphabet_scroll, false);

        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .vexpand(true)
            .hexpand(true)
            .build();

        let status_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(8)
            .build();
        status_box.append(&search_spinner);
        status_box.append(&empty_state);
        list_container.append(&status_box);
        list_container.append(&list_with_alphabet);

        let contacts_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .hexpand(true)
            .css_classes(vec!["unified-view-toolbar", "toolbar"])
            .build();

        let new_contact_btn = gtk::Button::builder()
            .icon_name("contact-new-symbolic")
            .tooltip_text("Neuer Kontakt (Strg+N)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&new_contact_btn, "contact-new-symbolic", "document-new-symbolic");
        set_accessible_label(&new_contact_btn, "Neuen Kontakt erstellen");

        let contact_list_for_new = contact_list.clone();
        let stored_contacts_for_new = Rc::clone(&stored_contacts);
        let empty_state_for_new = empty_state.clone();
        let selected_contact_for_new = Rc::clone(&selected_contact);
        let refresh_generation_for_new = Rc::clone(&refresh_generation);
        let category_model_for_new = category_model.clone();
        let category_selector_for_new = category_selector.clone();
        let selected_category_for_new = Rc::clone(&selected_category);
        let search_state_for_new = Rc::clone(&search_state);
        let page_weak_for_new_refresh = self.downgrade();
        new_contact_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_new.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_new);
                let empty_state_for_close = empty_state_for_new.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_new);
                let category_model_for_close = category_model_for_new.clone();
                let category_selector_for_close = category_selector_for_new.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_new);
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_new);
                let search_state_for_close = Rc::clone(&search_state_for_new);
                let page_weak_for_close = page_weak_for_new_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));
        self.imp()
            .new_contact_button
            .replace(Some(new_contact_btn.clone()));

        let new_list_btn = gtk::Button::builder()
            .icon_name("contact-group-symbolic")
            .tooltip_text("Neue Kontaktliste")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&new_list_btn, "contact-group-symbolic", "view-list-symbolic");
        set_accessible_label(&new_list_btn, "Neue Kontaktliste erstellen");
        let contact_list_for_new_list = contact_list.clone();
        let stored_contacts_for_new_list = Rc::clone(&stored_contacts);
        let empty_state_for_new_list = empty_state.clone();
        let selected_contact_for_new_list = Rc::clone(&selected_contact);
        let refresh_generation_for_new_list = Rc::clone(&refresh_generation);
        let category_model_for_new_list = category_model.clone();
        let category_selector_for_new_list = category_selector.clone();
        let selected_category_for_new_list = Rc::clone(&selected_category);
        let search_state_for_new_list = Rc::clone(&search_state);
        let page_weak_for_new_list_refresh = self.downgrade();
        new_list_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::ContactDialog::new_for_list();
                let root = page.root();
                if let Some(win) = root.and_then(|root| root.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_new_list.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_new_list);
                let empty_state_for_close = empty_state_for_new_list.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_new_list);
                let category_model_for_close = category_model_for_new_list.clone();
                let category_selector_for_close = category_selector_for_new_list.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_new_list);
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_new_list);
                let search_state_for_close = Rc::clone(&search_state_for_new_list);
                let page_weak_for_close = page_weak_for_new_list_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));
        set_accessible_label(&new_list_btn, "Neue Kontaktliste erstellen");
        self.imp()
            .new_list_button
            .replace(Some(new_list_btn.clone()));

        let print_list_btn = gtk::Button::builder()
            .tooltip_text("Sichtbare Kontakte drucken")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&print_list_btn, "Sichtbare Kontakte drucken");
        self.imp()
            .print_list_button
            .replace(Some(print_list_btn.clone()));
        let contact_list_for_list_print = contact_list.clone();
        let stored_contacts_for_list_print = Rc::clone(&stored_contacts);
        let search_state_for_list_print = Rc::clone(&search_state);
        print_list_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let visible = match visible_contact_records_checked(
                    &contact_list_for_list_print,
                    &stored_contacts_for_list_print,
                    &search_state_for_list_print,
                ) {
                    Ok(visible) => visible,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                match build_contact_list_print_document(&visible) {
                    Ok(doc) => {
                        let count = doc.contact_count;
                        let len = doc.html.len();
                        page.show_toast(&format!(
                            "Portables Listen-Druckdokument vorbereitet ({len} Zeichen, {count} Kontakte); Print-Portal ist Linux-Handoff"
                        ));
                    }
                    Err(error) => page.show_toast(&error),
                }
            }
        ));

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(40)
            .child(&sidebar_box)
            .vexpand(true)
            .build();

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("x-office-address-book-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Kontakte")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let drawer_total = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::End)
            .css_classes(vec!["sidebar-drawer-total", "dim-label"])
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);
        drawer_header.append(&drawer_total);
        self.imp().drawer_total.replace(Some(drawer_total));

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "contacts",
            &sidebar_outer_box,
        ));

        paned.set_start_child(Some(&sidebar_outer_box));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);

        // ── Contact Detail (Content) ────────────────────────────────────

        let detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .halign(gtk::Align::Center)
            .build();

        let detail_avatar = adw::Avatar::builder()
            .size(72)
            .text("")
            .show_initials(true)
            .build();

        let detail_name = gtk::Label::builder()
            .label("Kein Kontakt ausgewählt")
            .css_classes(vec!["title-1"])
            .build();

        let detail_title = gtk::Label::builder()
            .label("Adressbuchdaten werden nach der Synchronisation angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .build();

        let detail_extra = gtk::Label::builder()
            .label("Keine weiteren Kontaktdaten")
            .halign(gtk::Align::Start)
            .wrap(true)
            .selectable(true)
            .css_classes(vec!["body", "dim-label"])
            .build();

        header_box.append(&detail_avatar);
        header_box.append(&detail_name);
        header_box.append(&detail_title);

        let _actions_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_bottom(12)
            .build();

        let email_row1 = adw::ActionRow::builder()
            .title("Keine E-Mail-Adresse")
            .subtitle("Kein Kontakt ausgewählt")
            .build();

        let email_btn = gtk::Button::builder()
            .tooltip_text("E-Mail senden")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&email_btn, "E-Mail an den Kontakt senden");
        self.imp()
            .email_button
            .replace(Some(email_btn.clone()));

        let contact_list_for_email = contact_list.clone();
        let stored_contacts_for_email = Rc::clone(&stored_contacts);
        email_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_email,
                    &stored_contacts_for_email,
                ) {
                    Ok(selected) => selected,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                let recipient =
                    selected_contact_recipients(&selected, &stored_contacts_for_email.borrow());
                if recipient.trim().is_empty() {
                    page.show_toast("Die ausgewählten Kontakte haben keine E-Mail-Adresse");
                    return;
                }
                let dialog = crate::dialogs::ComposeDialog::to_recipient(&recipient);
                if let Some(window) = page.root().and_downcast::<gtk::Window>() {
                    dialog.set_transient_for(Some(&window));
                }
                dialog.present();
            }
        ));

        let edit_btn = gtk::Button::builder()
            .tooltip_text("Kontakt bearbeiten (Strg+E)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&edit_btn, "Kontakt bearbeiten");
        self.imp()
            .edit_button
            .replace(Some(edit_btn.clone()));

        let selected_contact_for_edit = Rc::clone(&selected_contact);
        let selected_contact_for_edit_refresh = Rc::clone(&selected_contact);
        let contact_list_for_edit = contact_list.clone();
        let stored_contacts_for_edit = Rc::clone(&stored_contacts);
        let empty_state_for_edit = empty_state.clone();
        let category_model_for_edit = category_model.clone();
        let category_selector_for_edit = category_selector.clone();
        let selected_category_for_edit = Rc::clone(&selected_category);
        let refresh_generation_for_edit = Rc::clone(&refresh_generation);
        let search_state_for_edit = Rc::clone(&search_state);
        let page_weak_for_edit_refresh = self.downgrade();
        edit_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(record) = selected_contact_for_edit.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if !contact_source_is_writable(&record.source_uid) {
                    page.show_toast(
                        "Dieser Kontakt stammt aus einer schreibgeschützten importierten Quelle",
                    );
                    return;
                }
                let dialog = crate::dialogs::ContactDialog::new_for_edit(&record);
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                let contact_list_for_close = contact_list_for_edit.clone();
                let stored_contacts_for_close = Rc::clone(&stored_contacts_for_edit);
                let empty_state_for_close = empty_state_for_edit.clone();
                let selected_contact_for_close = Rc::clone(&selected_contact_for_edit_refresh);
                let category_model_for_close = category_model_for_edit.clone();
                let category_selector_for_close = category_selector_for_edit.clone();
                let selected_category_for_close = Rc::clone(&selected_category_for_edit);
                let refresh_generation_for_close = Rc::clone(&refresh_generation_for_edit);
                let search_state_for_close = Rc::clone(&search_state_for_edit);
                let page_weak_for_close = page_weak_for_edit_refresh.clone();
                dialog.connect_close_request(move |_| {
                    refresh_contacts(
                        contact_list_for_close.clone(),
                        Rc::clone(&stored_contacts_for_close),
                        empty_state_for_close.clone(),
                        Rc::clone(&selected_contact_for_close),
                        category_model_for_close.clone(),
                        category_selector_for_close.clone(),
                        Rc::clone(&selected_category_for_close),
                        Rc::clone(&refresh_generation_for_close),
                        page_weak_for_close.clone(),
                        Rc::clone(&search_state_for_close),
                    );
                    glib::Propagation::Proceed
                });
                dialog.present();
            }
        ));

        let merge_btn = gtk::Button::builder()
            .tooltip_text("Dubletten derselben Quelle zusammenführen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&merge_btn, "Dubletten des Kontakts zusammenführen");
        self.imp()
            .merge_button
            .replace(Some(merge_btn.clone()));

        let selected_contact_for_merge = Rc::clone(&selected_contact);
        let stored_contacts_for_merge = Rc::clone(&stored_contacts);
        let contact_list_for_merge = contact_list.clone();
        let detail_name_for_merge = detail_name.clone();
        let detail_avatar_for_merge = detail_avatar.clone();
        let detail_title_for_merge = detail_title.clone();
        let detail_extra_for_merge = detail_extra.clone();
        let email_row_for_merge = email_row1.clone();
        let category_model_for_merge = category_model.clone();
        let category_selector_for_merge = category_selector.clone();
        let selected_category_for_merge = Rc::clone(&selected_category);
        let search_state_for_merge = Rc::clone(&search_state);
        let merge_btn_for_result = merge_btn.clone();
        merge_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let Some(primary) = selected_contact_for_merge.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                if !contact_source_is_writable(&primary.source_uid) {
                    page.show_toast(
                        "Dieser Kontakt stammt aus einer schreibgeschützten importierten Quelle",
                    );
                    return;
                }

                let duplicates = stored_contacts_for_merge
                    .borrow()
                    .iter()
                    .filter(|candidate| contact_records_are_duplicates(&primary, candidate))
                    .cloned()
                    .collect::<Vec<_>>();
                if duplicates.is_empty() {
                    page.show_toast("Für diesen Kontakt wurden keine Dubletten gefunden");
                    merge_btn_for_result.set_sensitive(false);
                    return;
                }

                let confirmation_message = format!(
                    "{} Duplikat(e) werden in „{}“ übernommen. Die Primärkarte bleibt erhalten; die Duplikate werden anschließend dauerhaft aus diesem Adressbuch entfernt.",
                    duplicates.len(),
                    primary.contact.fn_name
                );
                let primary_for_action = primary.clone();
                let duplicates_for_action = duplicates.clone();
                let selected_contact_for_action = Rc::clone(&selected_contact_for_merge);
                let stored_contacts_for_action = Rc::clone(&stored_contacts_for_merge);
                let contact_list_for_action = contact_list_for_merge.clone();
                let detail_avatar_for_action = detail_avatar_for_merge.clone();
                let detail_name_for_action = detail_name_for_merge.clone();
                let detail_title_for_action = detail_title_for_merge.clone();
                let detail_extra_for_action = detail_extra_for_merge.clone();
                let email_row_for_action = email_row_for_merge.clone();
                let category_model_for_action = category_model_for_merge.clone();
                let category_selector_for_action = category_selector_for_merge.clone();
                let selected_category_for_action = Rc::clone(&selected_category_for_merge);
                let merge_btn_for_action = merge_btn_for_result.clone();
                let page_weak_for_action = page.downgrade();
                let search_state_for_action = Rc::clone(&search_state_for_merge);
                crate::dialogs::run_destructive_action(
                    &detail_name_for_merge,
                    "Dubletten zusammenführen?",
                    &confirmation_message,
                    "Zusammenführen",
                    move || {
                        let primary_for_worker = primary_for_action.clone();
                        let duplicates_for_worker = duplicates_for_action.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = EBookService::new()
                                .merge_contacts(&primary_for_worker, &duplicates_for_worker);
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Contact merge result was dropped");
                            }
                        });

                        let page_weak_for_result = page_weak_for_action.clone();
                        glib::spawn_future_local(async move {
                            match receiver.recv().await {
                                Ok(Ok(result)) => {
                                    let Some(vcard) = result.vcard else {
                                        show_toast_for_widget(
                                            &detail_name_for_action,
                                            "Zusammenführung ohne bestätigte vCard vom Backend",
                                        );
                                        return;
                                    };
                                    let Some(contact) =
                                        VCardContact::parse_vcard_string(&vcard)
                                    else {
                                        show_toast_for_widget(
                                            &detail_name_for_action,
                                            "Die zusammengeführte vCard konnte nicht gelesen werden",
                                        );
                                        return;
                                    };
                                    let merged = EBookContactRecord {
                                        source_uid: primary_for_action.source_uid.clone(),
                                        source_display_name: primary_for_action
                                            .source_display_name
                                            .clone(),
                                        uid: confirmed_contact_uid(
                                            &result.uid,
                                            &primary_for_action.uid,
                                        ),
                                        vcard,
                                        contact,
                                    };
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        // Remove any automatic GTK row selection before
                                        // rebuilding the primary row.  Otherwise ListBox can
                                        // select an unrelated neighbour and the search
                                        // generation would restore that contact instead.
                                        clear_contact_selection(&page, &contact_list_for_action);
                                    }
                                    remove_contact_list_row(
                                        &contact_list_for_action,
                                        &primary_for_action.source_uid,
                                        &primary_for_action.uid,
                                    );
                                    for duplicate in &duplicates_for_action {
                                        remove_contact_list_row(
                                            &contact_list_for_action,
                                            &duplicate.source_uid,
                                            &duplicate.uid,
                                        );
                                    }
                                    {
                                        let mut contacts = stored_contacts_for_action.borrow_mut();
                                        contacts.retain(|candidate| {
                                            candidate.source_uid != primary_for_action.source_uid
                                                || (candidate.uid != primary_for_action.uid
                                                    && !duplicates_for_action.iter().any(|duplicate| {
                                                        duplicate.source_uid == candidate.source_uid
                                                            && duplicate.uid == candidate.uid
                                                    }))
                                        });
                                        contacts.push(merged.clone());
                                    }
                                    append_contact_row(&contact_list_for_action, &merged);
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                                            remove_contact_card(flow, &primary_for_action.source_uid, &primary_for_action.uid);
                                            for duplicate in &duplicates_for_action {
                                                remove_contact_card(flow, &duplicate.source_uid, &duplicate.uid);
                                            }
                                            append_contact_card(flow, &merged);
                                        }
                                    }
                                    mark_duplicate_rows(
                                        &contact_list_for_action,
                                        &stored_contacts_for_action,
                                    );
                                    refresh_contact_category_model(
                                        &category_model_for_action,
                                        &category_selector_for_action,
                                        &stored_contacts_for_action,
                                        &selected_category_for_action,
                                    );
                                    update_contact_detail(
                                        &merged,
                                        &detail_avatar_for_action,
                                        &detail_name_for_action,
                                        &detail_title_for_action,
                                        &detail_extra_for_action,
                                        &email_row_for_action,
                                    );
                                    *selected_contact_for_action.borrow_mut() = Some(merged.clone());
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        page.imp().selected_identity.replace(Some(
                                            ContactIdentity::new(
                                                merged.source_uid.clone(),
                                                merged.uid.clone(),
                                            ),
                                        ));
                                    }
                                    merge_btn_for_action.set_sensitive(
                                        contact_has_mergeable_duplicate(
                                            &merged,
                                            &stored_contacts_for_action.borrow(),
                                        ),
                                    );
                                    if let Some(page) = page_weak_for_result.upgrade() {
                                        reapply_active_contact_search(
                                            page.downgrade(),
                                            Rc::clone(&search_state_for_action),
                                            Rc::clone(&stored_contacts_for_action),
                                        );
                                        page.show_toast(&format!(
                                            "{} Dublette(n) in „{}“ zusammengeführt",
                                            duplicates_for_action.len(),
                                            merged.contact.fn_name
                                        ));
                                    }
                                }
                                Ok(Err(error)) => show_toast_for_widget(
                                    &detail_name_for_action,
                                    &format!("Dubletten konnten nicht zusammengeführt werden: {error}"),
                                ),
                                Err(error) => tracing::debug!(%error, "Contact merge channel closed"),
                            }
                        });
                    },
                );
            }
        ));

        let delete_btn = gtk::Button::builder()
            .tooltip_text("Kontakt löschen (Entf)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&delete_btn, "Kontakt löschen");
        self.imp()
            .delete_button
            .replace(Some(delete_btn.clone()));

        let export_btn = gtk::Button::builder()
            .tooltip_text("Ausgewählte Kontakte exportieren (vCard, CSV, LDIF)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&export_btn, "Ausgewählte Kontakte exportieren");
        self.imp()
            .export_button
            .replace(Some(export_btn.clone()));
        let contact_list_for_export = contact_list.clone();
        let stored_contacts_for_export = Rc::clone(&stored_contacts);
        let export_btn_for_export = export_btn.clone();
        export_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_export,
                    &stored_contacts_for_export,
                ) {
                    Ok(selected) => selected,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                if selected.is_empty() {
                    page.show_toast("Keine Kontakte zum Exportieren ausgewählt");
                    return;
                }
                let Some(root) = page.root().and_downcast::<gtk::Window>() else {
                    page.show_toast("Kein Fenster für den Export verfügbar");
                    return;
                };

                let filter_vcf = gtk::FileFilter::new();
                filter_vcf.set_name(Some("vCard (*.vcf)"));
                filter_vcf.add_pattern("*.vcf");
                filter_vcf.add_pattern("*.vcard");

                let filter_csv = gtk::FileFilter::new();
                filter_csv.set_name(Some("Kommagetrennte Werte (*.csv)"));
                filter_csv.add_pattern("*.csv");

                let filter_ldif = gtk::FileFilter::new();
                filter_ldif.set_name(Some("LDAP Data Interchange Format (*.ldif)"));
                filter_ldif.add_pattern("*.ldif");
                filter_ldif.add_pattern("*.ldi");

                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter_vcf);
                filters.append(&filter_csv);
                filters.append(&filter_ldif);

                let initial_name = if selected.len() == 1 {
                    let name = &selected[0].contact.fn_name;
                    let safe = name
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        .collect::<String>();
                    if safe.is_empty() {
                        "kontakt.vcf".to_string()
                    } else {
                        format!("{safe}.vcf")
                    }
                } else {
                    "kontakte.vcf".to_string()
                };

                let file_dialog = gtk::FileDialog::builder()
                    .title("Kontakte exportieren")
                    .filters(&filters)
                    .default_filter(&filter_vcf)
                    .initial_name(&initial_name)
                    .modal(true)
                    .build();
                let export_button = export_btn_for_export.clone();
                let selected_for_dialog = selected.clone();
                file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let format = if let Some(path) = file.path() {
                        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                        if ext.eq_ignore_ascii_case("csv") {
                            ContactExportFormat::Csv
                        } else if ext.eq_ignore_ascii_case("ldif")
                            || ext.eq_ignore_ascii_case("ldi")
                        {
                            ContactExportFormat::Ldif
                        } else {
                            ContactExportFormat::VCard
                        }
                    } else {
                        ContactExportFormat::VCard
                    };
                    match plan_contact_export(&selected_for_dialog, format) {
                        Ok(plan) => save_contact_export_async(&export_button, file, plan),
                        Err(err) => show_toast_for_widget(&export_button, &err),
                    }
                });
            }
        ));

        let print_btn = gtk::Button::builder()
            .tooltip_text("Ausgewählte Kontakte drucken (Strg+P)")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&print_btn, "Ausgewählte Kontakte drucken");
        self.imp()
            .print_button
            .replace(Some(print_btn.clone()));
        let contact_list_for_print = contact_list.clone();
        let stored_contacts_for_print = Rc::clone(&stored_contacts);
        print_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_print,
                    &stored_contacts_for_print,
                ) {
                    Ok(selected) => selected,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };
                match build_contact_print_document(&selected) {
                    Ok(doc) => {
                        let count = doc.contact_count;
                        let len = doc.html.len();
                        let label = if count == 1 {
                            format!(
                                "Portables Druckdokument vorbereitet ({len} Zeichen, 1 Kontakt); Print-Portal ist Linux-Handoff"
                            )
                        } else {
                            format!(
                                "Portables Druckdokument vorbereitet ({len} Zeichen, {count} Kontakte); Print-Portal ist Linux-Handoff"
                            )
                        };
                        page.show_toast(&label);
                    }
                    Err(error) => page.show_toast(&error),
                }
            }
        ));

        let copy_to_book_btn = gtk::Button::builder()
            .tooltip_text("In Adressbuch kopieren…")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&copy_to_book_btn, "In Adressbuch kopieren");
        self.imp()
            .copy_to_book_button
            .replace(Some(copy_to_book_btn.clone()));

        let move_to_book_btn = gtk::Button::builder()
            .tooltip_text("In Adressbuch verschieben…")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&move_to_book_btn, "In Adressbuch verschieben");
        self.imp()
            .move_to_book_button
            .replace(Some(move_to_book_btn.clone()));

        let transfer_ctx = ContactTransferUiContext {
            contact_list: contact_list.clone(),
            stored_contacts: Rc::clone(&stored_contacts),
            selected_contact: Rc::clone(&selected_contact),
            detail_avatar: detail_avatar.clone(),
            detail_name: detail_name.clone(),
            detail_title: detail_title.clone(),
            detail_extra: detail_extra.clone(),
            email_btn: email_btn.clone(),
            export_btn: export_btn.clone(),
            print_btn: print_btn.clone(),
            edit_btn: edit_btn.clone(),
            merge_btn: merge_btn.clone(),
            delete_btn: delete_btn.clone(),
            copy_to_book_btn: copy_to_book_btn.clone(),
            move_to_book_btn: move_to_book_btn.clone(),
            category_model: category_model.clone(),
            category_selector: category_selector.clone(),
            selected_category: Rc::clone(&selected_category),
            empty_state: empty_state.clone(),
            search_state: Rc::clone(&search_state),
        };

        let contact_list_for_copy_book = contact_list.clone();
        let stored_contacts_for_copy_book = Rc::clone(&stored_contacts);
        let selected_contact_for_copy_book = Rc::clone(&selected_contact);
        let book_model_for_copy_book = book_model.clone();
        let book_source_uids_for_copy_book = Rc::clone(&book_source_uids);
        let ctx_for_copy_book = transfer_ctx.clone();
        let detail_name_for_copy_book = detail_name.clone();
        copy_to_book_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_copy_book,
                    &stored_contacts_for_copy_book,
                ) {
                    Ok(selected) if !selected.is_empty() => selected,
                    _ => {
                        if let Some(contact) = selected_contact_for_copy_book.borrow().clone() {
                            vec![contact]
                        } else {
                            page.show_toast("Kein Kontakt ausgewählt");
                            return;
                        }
                    }
                };
                run_contact_transfer_dialog(
                    &detail_name_for_copy_book,
                    &page,
                    ContactTransferKind::Copy,
                    selected,
                    &book_model_for_copy_book,
                    &book_source_uids_for_copy_book.borrow(),
                    ctx_for_copy_book.clone(),
                );
            }
        ));

        let contact_list_for_move_book = contact_list.clone();
        let stored_contacts_for_move_book = Rc::clone(&stored_contacts);
        let selected_contact_for_move_book = Rc::clone(&selected_contact);
        let book_model_for_move_book = book_model.clone();
        let book_source_uids_for_move_book = Rc::clone(&book_source_uids);
        let ctx_for_move_book = transfer_ctx.clone();
        let detail_name_for_move_book = detail_name.clone();
        move_to_book_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_move_book,
                    &stored_contacts_for_move_book,
                ) {
                    Ok(selected) if !selected.is_empty() => selected,
                    _ => {
                        if let Some(contact) = selected_contact_for_move_book.borrow().clone() {
                            vec![contact]
                        } else {
                            page.show_toast("Kein Kontakt ausgewählt");
                            return;
                        }
                    }
                };
                run_contact_transfer_dialog(
                    &detail_name_for_move_book,
                    &page,
                    ContactTransferKind::Move,
                    selected,
                    &book_model_for_move_book,
                    &book_source_uids_for_move_book.borrow(),
                    ctx_for_move_book.clone(),
                );
            }
        ));

        let selected_contact_for_delete = Rc::clone(&selected_contact);
        let stored_contacts_for_delete = Rc::clone(&stored_contacts);
        let contact_list_for_delete = contact_list.clone();
        let detail_name_for_delete = detail_name.clone();
        let detail_title_for_delete = detail_title.clone();
        let detail_avatar_for_delete = detail_avatar.clone();
        let detail_extra_for_delete = detail_extra.clone();
        let empty_state_for_delete = empty_state.clone();
        let category_model_for_delete = category_model.clone();
        let category_selector_for_delete = category_selector.clone();
        let selected_category_for_delete = Rc::clone(&selected_category);
        let email_btn_for_delete = email_btn.clone();
        let export_btn_for_delete = export_btn.clone();
        let print_btn_for_delete = print_btn.clone();
        let edit_btn_for_delete = edit_btn.clone();
        let merge_btn_for_delete = merge_btn.clone();
        let delete_btn_for_delete = delete_btn.clone();
        let copy_to_book_btn_for_delete = copy_to_book_btn.clone();
        let move_to_book_btn_for_delete = move_to_book_btn.clone();
        let search_state_for_delete = Rc::clone(&search_state);
        delete_btn.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let selected = match selected_contact_records_checked(
                    &contact_list_for_delete,
                    &stored_contacts_for_delete,
                ) {
                    Ok(selected) if !selected.is_empty() => selected,
                    _ => {
                        if let Some(contact) = selected_contact_for_delete.borrow().clone() {
                            vec![contact]
                        } else {
                            page.show_toast("Kein Kontakt ausgewählt");
                            return;
                        }
                    }
                };

                let plan = match plan_contact_delete_batch(&selected) {
                    Ok(plan) => plan,
                    Err(error) => {
                        page.show_toast(&error);
                        return;
                    }
                };

                let plan_for_action = plan.clone();
                let selected_contact_for_action = Rc::clone(&selected_contact_for_delete);
                let stored_contacts_for_action = Rc::clone(&stored_contacts_for_delete);
                let contact_list_for_action = contact_list_for_delete.clone();
                let detail_name_for_action = detail_name_for_delete.clone();
                let detail_title_for_action = detail_title_for_delete.clone();
                let detail_avatar_for_action = detail_avatar_for_delete.clone();
                let detail_extra_for_action = detail_extra_for_delete.clone();
                let empty_state_for_action = empty_state_for_delete.clone();
                let category_model_for_action = category_model_for_delete.clone();
                let category_selector_for_action = category_selector_for_delete.clone();
                let selected_category_for_action = Rc::clone(&selected_category_for_delete);
                let email_btn_for_action = email_btn_for_delete.clone();
                let export_btn_for_action = export_btn_for_delete.clone();
                let print_btn_for_action = print_btn_for_delete.clone();
                let edit_btn_for_action = edit_btn_for_delete.clone();
                let merge_btn_for_action = merge_btn_for_delete.clone();
                let delete_btn_for_action = delete_btn_for_delete.clone();
                let copy_to_book_btn_for_action = copy_to_book_btn_for_delete.clone();
                let move_to_book_btn_for_action = move_to_book_btn_for_delete.clone();
                let page_weak_for_action = page.downgrade();
                let search_state_for_action = Rc::clone(&search_state_for_delete);
                crate::dialogs::run_destructive_action(
                    &detail_name_for_delete,
                    &plan.confirmation.title,
                    &plan.confirmation.description,
                    "Löschen",
                    move || {
                        let plan_for_worker = plan_for_action.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let service = EBookService::new();
                            let mut result = ContactDeleteWorkerResult {
                                removed: Vec::with_capacity(plan_for_worker.items.len()),
                                failure: None,
                            };
                            for item in plan_for_worker.items {
                                if !contact_source_is_writable(&item.identity.source_uid) {
                                    result.failure = Some((
                                        item.identity,
                                        "Die Adressbuchquelle ist unmittelbar vor dem Löschen nicht mehr schreibfähig"
                                            .to_string(),
                                    ));
                                    break;
                                }
                                match service.remove_contact(&item.identity.source_uid, &item.identity.uid) {
                                    Ok(confirmed) => {
                                        let confirmed_uid = confirmed_contact_uid(&confirmed.uid, &item.identity.uid);
                                        if confirmed_uid == item.identity.uid {
                                            result.removed.push(item.identity);
                                        } else {
                                            result.failure = Some((
                                                item.identity,
                                                "Das Backend bestätigte das Löschen eines anderen Kontakts".to_string(),
                                            ));
                                            break;
                                        }
                                    }
                                    Err(error) => {
                                        result.failure = Some((item.identity, error.to_string()));
                                        break;
                                    }
                                }
                            }
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Contact deletion result was dropped");
                            }
                        });

                        let selected_contact_for_result = Rc::clone(&selected_contact_for_action);
                        let stored_contacts_for_result = Rc::clone(&stored_contacts_for_action);
                        let contact_list_for_result = contact_list_for_action.clone();
                        let detail_name_for_result = detail_name_for_action.clone();
                        let detail_title_for_result = detail_title_for_action.clone();
                        let detail_avatar_for_result = detail_avatar_for_action.clone();
                        let detail_extra_for_result = detail_extra_for_action.clone();
                        let empty_state_for_result = empty_state_for_action.clone();
                        let category_model_for_result = category_model_for_action.clone();
                        let category_selector_for_result = category_selector_for_action.clone();
                        let selected_category_for_result = Rc::clone(&selected_category_for_action);
                        let email_btn_for_result = email_btn_for_action.clone();
                        let export_btn_for_result = export_btn_for_action.clone();
                        let print_btn_for_result = print_btn_for_action.clone();
                        let edit_btn_for_result = edit_btn_for_action.clone();
                        let merge_btn_for_result = merge_btn_for_action.clone();
                        let delete_btn_for_result = delete_btn_for_action.clone();
                        let copy_to_book_btn_for_result = copy_to_book_btn_for_action.clone();
                        let move_to_book_btn_for_result = move_to_book_btn_for_action.clone();
                        let page_weak_for_result = page_weak_for_action.clone();
                        let search_state_for_result = Rc::clone(&search_state_for_action);
                        glib::spawn_future_local(async move {
                            match receiver.recv().await {
                                Ok(result) => {
                                    let removed_count = result.removed.len();
                                    if removed_count > 0 {
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            clear_contact_selection(&page, &contact_list_for_result);
                                        }
                                        for removed_id in &result.removed {
                                            remove_contact_list_row(
                                                &contact_list_for_result,
                                                &removed_id.source_uid,
                                                &removed_id.uid,
                                            );
                                        }
                                        let updated = reduce_contact_delete_result(
                                            &stored_contacts_for_result.borrow(),
                                            &result.removed,
                                        );
                                        *stored_contacts_for_result.borrow_mut() = updated;

                                        let selected_is_deleted = selected_contact_for_result
                                            .borrow()
                                            .as_ref()
                                            .map(|c| {
                                                result.removed.iter().any(|r| {
                                                    r.source_uid == c.source_uid && r.uid == c.uid
                                                })
                                            })
                                            .unwrap_or(false);

                                        if selected_is_deleted {
                                            *selected_contact_for_result.borrow_mut() = None;
                                            detail_avatar_for_result.set_text(Some(""));
                                            detail_avatar_for_result
                                                .set_custom_image(None::<&gdk::Texture>);
                                            detail_name_for_result
                                                .set_label("Kein Kontakt ausgewählt");
                                            detail_title_for_result.set_label(
                                                "Adressbuchdaten werden nach der Synchronisation angezeigt",
                                            );
                                            detail_extra_for_result
                                                .set_label("Keine weiteren Kontaktdaten");
                                        }

                                        mark_duplicate_rows(
                                            &contact_list_for_result,
                                            &stored_contacts_for_result,
                                        );
                                        empty_state_for_result.set_visible(
                                            stored_contacts_for_result.borrow().is_empty(),
                                        );
                                        if stored_contacts_for_result.borrow().is_empty() {
                                            empty_state_for_result
                                                .set_label("Keine Kontakte im Adressbuch geladen");
                                        }
                                        refresh_contact_category_model(
                                            &category_model_for_result,
                                            &category_selector_for_result,
                                            &stored_contacts_for_result,
                                            &selected_category_for_result,
                                        );
                                        update_contact_action_sensitivity(
                                            &email_btn_for_result,
                                            &export_btn_for_result,
                                            &edit_btn_for_result,
                                            &merge_btn_for_result,
                                            &delete_btn_for_result,
                                            Some(&print_btn_for_result),
                                            None,
                                            Some(&copy_to_book_btn_for_result),
                                            Some(&move_to_book_btn_for_result),
                                            &selected_contact_for_result,
                                            &stored_contacts_for_result,
                                        );
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            page.update_drawer_total(stored_contacts_for_result.borrow().len());
                                            reapply_active_contact_search(
                                                page.downgrade(),
                                                Rc::clone(&search_state_for_result),
                                                Rc::clone(&stored_contacts_for_result),
                                            );
                                        }
                                    }

                                    if let Some((failed_id, error)) = result.failure {
                                        let message = if removed_count > 0 {
                                            format!(
                                                "{removed_count} Kontakt(e) gelöscht; Fehler beim Löschen von {}: {error}",
                                                failed_id.uid
                                            )
                                        } else {
                                            format!("Kontakt konnte nicht gelöscht werden: {error}")
                                        };
                                        show_toast_for_widget(&detail_name_for_result, &message);
                                    } else if removed_count == 1 {
                                        let uid = &result.removed[0].uid;
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            page.show_toast(&format!("Kontakt gelöscht ({uid})"));
                                        }
                                    } else if removed_count > 1 {
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            page.show_toast(&format!("{removed_count} Kontakte gelöscht"));
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "Contact deletion channel closed");
                                }
                            }
                        });
                    },
                );
            }
        ));

        // Match Evolution's contact-row context menu with the actions already
        // exposed in the detail header.  Selecting the row first also makes
        // the menu useful when the split view is collapsed on a narrow window.
        let email_btn_for_context = email_btn.clone();
        let edit_btn_for_context = edit_btn.clone();
        let export_btn_for_context = export_btn.clone();
        let print_btn_for_context = print_btn.clone();
        let copy_to_book_btn_for_context = copy_to_book_btn.clone();
        let move_to_book_btn_for_context = move_to_book_btn.clone();
        let merge_btn_for_context = merge_btn.clone();
        let delete_btn_for_context = delete_btn.clone();
        let selected_contact_for_context = Rc::clone(&selected_contact);
        let stored_contacts_for_context = Rc::clone(&stored_contacts);
        let context_click = gtk::GestureClick::builder().button(3).build();
        let contact_list_weak_for_context = contact_list.downgrade();
        context_click.connect_pressed(move |_, _, x, y| {
            let Some(contact_list_for_context) = contact_list_weak_for_context.upgrade() else {
                return;
            };
            let Some(row) = contact_list_for_context.row_at_y(y as i32) else {
                return;
            };
            // A context menu targets its clicked contact.  Do not leave a
            // stale multi-selection attached to a menu made for one row.
            contact_list_for_context.unselect_all();
            contact_list_for_context.select_row(Some(&row));

            let popover = gtk::Popover::builder()
                .has_arrow(false)
                .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                .build();
            let menu_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(6)
                .margin_end(6)
                .margin_top(6)
                .margin_bottom(6)
                .build();

            let email = gtk::Button::with_label("E-Mail senden");
            email.set_halign(gtk::Align::Fill);
            email.add_css_class("flat");
            let popover_for_email = popover.clone();
            let email_button_for_menu = email_btn_for_context.clone();
            email.connect_clicked(move |_| {
                popover_for_email.popdown();
                email_button_for_menu.activate();
            });
            menu_box.append(&email);

            let edit = gtk::Button::with_label("Kontakt bearbeiten");
            edit.set_halign(gtk::Align::Fill);
            edit.add_css_class("flat");
            let popover_for_edit = popover.clone();
            let edit_button_for_menu = edit_btn_for_context.clone();
            edit.connect_clicked(move |_| {
                popover_for_edit.popdown();
                edit_button_for_menu.activate();
            });
            menu_box.append(&edit);

            let export = gtk::Button::with_label("Exportieren (vCard, CSV, LDIF)…");
            export.set_halign(gtk::Align::Fill);
            export.add_css_class("flat");
            let popover_for_export = popover.clone();
            let export_button_for_menu = export_btn_for_context.clone();
            export.connect_clicked(move |_| {
                popover_for_export.popdown();
                export_button_for_menu.activate();
            });
            menu_box.append(&export);

            let print = gtk::Button::with_label("Drucken…");
            print.set_halign(gtk::Align::Fill);
            print.add_css_class("flat");
            let popover_for_print = popover.clone();
            let print_button_for_menu = print_btn_for_context.clone();
            print.connect_clicked(move |_| {
                popover_for_print.popdown();
                print_button_for_menu.activate();
            });
            menu_box.append(&print);

            let copy_to_book = gtk::Button::with_label("In Adressbuch kopieren…");
            copy_to_book.set_halign(gtk::Align::Fill);
            copy_to_book.add_css_class("flat");
            let popover_for_copy_to_book = popover.clone();
            let copy_to_book_button_for_menu = copy_to_book_btn_for_context.clone();
            copy_to_book.connect_clicked(move |_| {
                popover_for_copy_to_book.popdown();
                copy_to_book_button_for_menu.activate();
            });
            menu_box.append(&copy_to_book);

            let move_to_book = gtk::Button::with_label("In Adressbuch verschieben…");
            move_to_book.set_halign(gtk::Align::Fill);
            move_to_book.add_css_class("flat");
            let popover_for_move_to_book = popover.clone();
            let move_to_book_button_for_menu = move_to_book_btn_for_context.clone();
            move_to_book.connect_clicked(move |_| {
                popover_for_move_to_book.popdown();
                move_to_book_button_for_menu.activate();
            });
            menu_box.append(&move_to_book);

            let merge = gtk::Button::with_label("Dubletten zusammenführen");
            merge.set_halign(gtk::Align::Fill);
            merge.add_css_class("flat");
            let popover_for_merge = popover.clone();
            let merge_button_for_menu = merge_btn_for_context.clone();
            merge.connect_clicked(move |_| {
                popover_for_merge.popdown();
                merge_button_for_menu.activate();
            });
            menu_box.append(&merge);

            let delete = gtk::Button::with_label("Kontakt löschen");
            delete.set_halign(gtk::Align::Fill);
            delete.add_css_class("flat");
            delete.add_css_class("destructive-action");
            let popover_for_delete = popover.clone();
            let delete_button_for_menu = delete_btn_for_context.clone();
            delete.connect_clicked(move |_| {
                popover_for_delete.popdown();
                delete_button_for_menu.activate();
            });
            menu_box.append(&delete);

            // Context actions use the same selection/source capability
            // calculation as the header.  This is deliberately applied only
            // after the row was selected, so imported/read-only records fail
            // closed in both action surfaces.
            let action_state = context_action_state(
                selected_contact_for_context.borrow().as_ref(),
                &stored_contacts_for_context.borrow(),
            );
            apply_contact_action_state(
                action_state,
                &email,
                &export,
                &edit,
                &merge,
                &delete,
                Some(&print),
                None,
                Some(&copy_to_book),
                Some(&move_to_book),
            );

            popover.set_child(Some(&menu_box));
            popover.set_parent(&contact_list_for_context);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        contact_list.add_controller(context_click);

        let preview_visible = AppSettings::try_new()
            .map(|s| s.contact_preview_visible())
            .unwrap_or(true);
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Kontaktvorschau ein-/ausblenden (Strg+M)")
            .active(preview_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&preview_toggle, "Kontaktvorschau ein- oder ausblenden");
        self.imp()
            .contact_preview_toggle
            .replace(Some(preview_toggle.clone()));

        let detail_box_for_toggle = detail_box.clone();
        preview_toggle.connect_toggled(move |toggle| {
            let is_active = toggle.is_active();
            if let Some(settings) = AppSettings::try_new() {
                settings.set_contact_preview_visible(is_active);
            }
            detail_box_for_toggle.set_visible(is_active);
        });

        let sidebar_visible = true;
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Seitenleiste ein-/ausblenden (F9)")
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, "Seitenleiste ein- oder ausblenden");
        self.imp()
            .contacts_sidebar_toggle
            .replace(Some(sidebar_toggle.clone()));

        let sidebar_for_toggle = sidebar_outer_box.clone();
        sidebar_toggle.connect_toggled(move |toggle| {
            let is_active = toggle.is_active();
            sidebar_for_toggle.set_visible(is_active);
        });

        let list_key_controller = gtk::EventControllerKey::new();
        let delete_btn_for_key = delete_btn.clone();
        let print_btn_for_key = print_btn.clone();
        let print_list_btn_for_key = print_list_btn.clone();
        let contact_list_for_key = contact_list.clone();
        let preview_toggle_for_key = preview_toggle.clone();
        list_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if (key == gtk::gdk::Key::Delete
                || key == gtk::gdk::Key::BackSpace
                || key == gtk::gdk::Key::KP_Delete)
                && modifiers
                    .difference(gtk::gdk::ModifierType::LOCK_MASK)
                    .is_empty()
            {
                if delete_btn_for_key.is_sensitive() {
                    delete_btn_for_key.activate();
                    return glib::Propagation::Stop;
                }
            } else if (key == gtk::gdk::Key::a || key == gtk::gdk::Key::A)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                contact_list_for_key.select_all();
                return glib::Propagation::Stop;
            } else if (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                preview_toggle_for_key.set_active(!preview_toggle_for_key.is_active());
                return glib::Propagation::Stop;
            } else if (key == gtk::gdk::Key::p || key == gtk::gdk::Key::P)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                if print_btn_for_key.is_sensitive() {
                    print_btn_for_key.activate();
                    return glib::Propagation::Stop;
                } else if print_list_btn_for_key.is_sensitive() {
                    print_list_btn_for_key.activate();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        contact_list.add_controller(list_key_controller);

        let page_key_controller = gtk::EventControllerKey::new();
        let preview_toggle_for_page = preview_toggle.clone();
        let sidebar_toggle_for_key = sidebar_toggle.clone();
        page_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_key.set_active(!sidebar_toggle_for_key.is_active());
                return glib::Propagation::Stop;
            }
            if (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                preview_toggle_for_page.set_active(!preview_toggle_for_page.is_active());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.add_controller(page_key_controller);

        // Initialize toolbar button modes (desktop: pure text, mobile: pure icon)
        let is_compact_initial = self.imp().is_mobile.get() || (self.width() > 0 && self.width() < 820);
        self.apply_toolbar_button_modes(is_compact_initial);

        self.connect_root_notify(move |page| {
            if let Some(win) = page.root().and_downcast::<gtk::Window>() {
                let page_weak = page.downgrade();
                win.connect_default_width_notify(move |_| {
                    if let Some(page) = page_weak.upgrade() {
                        page.sync_toolbar_button_labels();
                    }
                });
            }
        });

        let left_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Start)
            .build();
        // 1. Primary actions: Edit, Email, Delete
        left_box.append(&edit_btn);
        left_box.append(&email_btn);
        left_box.append(&delete_btn);
        left_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        // 2. Output actions: Print, Print List, Export
        left_box.append(&print_btn);
        left_box.append(&print_list_btn);
        left_box.append(&export_btn);
        left_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        // 3. Organization & Maintenance actions: Merge, Copy, Move
        left_box.append(&merge_btn);
        left_box.append(&copy_to_book_btn);
        left_box.append(&move_to_book_btn);

        let contacts_search_bar = gtk::SearchBar::builder()
            .search_mode_enabled(false)
            .key_capture_widget(self)
            .build();
        contacts_search_bar.connect_entry(&search_entry);
        self.imp()
            .contact_search_bar
            .replace(Some(contacts_search_bar.clone()));

        let search_content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        search_entry.set_width_chars(32);
        search_entry.set_valign(gtk::Align::Center);
        quick_mode.set_valign(gtk::Align::Center);
        advanced_search_button.set_valign(gtk::Align::Center);

        search_content_box.append(&search_entry);
        search_content_box.append(&quick_mode);
        search_content_box.append(&advanced_search_button);
        contacts_search_bar.set_child(Some(&search_content_box));

        let entry_for_search_mode = search_entry.clone();
        let page_weak_for_search_mode = self.downgrade();
        contacts_search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            let is_mode = bar.is_search_mode();
            if is_mode {
                entry_for_search_mode.grab_focus();
            } else if !entry_for_search_mode.text().is_empty() {
                entry_for_search_mode.set_text("");
            }
            if let Some(page) = page_weak_for_search_mode.upgrade() {
                if let Some(window) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                    if let Some(btn) = window.search_button() {
                        if btn.is_active() != is_mode {
                            btn.set_active(is_mode);
                        }
                    }
                }
            }
        });

        let bar_for_stop = contacts_search_bar.clone();
        search_entry.connect_stop_search(move |_| {
            bar_for_stop.set_search_mode(false);
        });

        let right_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .halign(gtk::Align::End)
            .build();
        let view_switcher_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .valign(gtk::Align::Center)
            .margin_start(4)
            .margin_end(4)
            .build();

        let card_view_btn = gtk::ToggleButton::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text("Kartenansicht")
            .active(true)
            .css_classes(vec!["flat"])
            .build();

        let list_view_btn = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Listenansicht")
            .group(&card_view_btn)
            .css_classes(vec!["flat"])
            .build();

        let view_stack_for_card = view_stack.clone();
        let page_weak_for_card = self.downgrade();
        card_view_btn.connect_toggled(glib::clone!(
            #[weak]
            view_stack_for_card,
            move |btn| {
                if btn.is_active() {
                    view_stack_for_card.set_visible_child_name("cards");
                    if let Some(page) = page_weak_for_card.upgrade() {
                        if let Some(paned) = page.imp().contact_preview_paned.borrow().as_ref() {
                            if paned.position() < 600 {
                                paned.set_position(650);
                            }
                        }
                    }
                }
            }
        ));

        let view_stack_for_list = view_stack.clone();
        list_view_btn.connect_toggled(glib::clone!(
            #[weak]
            view_stack_for_list,
            move |btn| {
                if btn.is_active() {
                    view_stack_for_list.set_visible_child_name("list");
                }
            }
        ));

        view_switcher_box.append(&card_view_btn);
        view_switcher_box.append(&list_view_btn);

        right_box.append(&view_switcher_box);

        let spacer = gtk::Box::builder().hexpand(true).build();
        contacts_toolbar.append(&left_box);
        contacts_toolbar.append(&spacer);
        contacts_toolbar.append(&right_box);

        let contacts_toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .child(&contacts_toolbar)
            .build();
        contacts_toolbar_scroll.set_visible(
            AppSettings::try_new()
                .map(|settings| settings.contact_show_toolbar())
                .unwrap_or(true),
        );
        self.imp()
            .toolbar
            .replace(Some(contacts_toolbar_scroll.clone().upcast()));

        detail_box.append(&header_box);
        detail_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let info_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let info_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let email_group = adw::PreferencesGroup::builder().title("E-Mail").build();

        let copy_btn1 = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .tooltip_text("Kopieren")
            .build();
        set_accessible_label(&copy_btn1, "E-Mail-Adresse kopieren");
        let selected_contact_for_copy = Rc::clone(&selected_contact);
        let stored_contacts_for_copy = Rc::clone(&stored_contacts);
        copy_btn1.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| {
                let Some(record) = selected_contact_for_copy.borrow().clone() else {
                    page.show_toast("Kein Kontakt ausgewählt");
                    return;
                };
                let email = if record.contact.is_list {
                    if record.contact.list_show_addresses {
                        contact_recipient_text_with_records(
                            &record.contact,
                            &stored_contacts_for_copy.borrow(),
                        )
                    } else {
                        String::new()
                    }
                } else {
                    contact_email_text(&record.contact)
                };
                if email.trim().is_empty() {
                    page.show_toast("Keine E-Mail-Adresse zum Kopieren vorhanden");
                    return;
                }
                button.display().clipboard().set_text(&email);
                page.show_toast("E-Mail-Adresse kopiert");
            }
        ));
        email_row1.add_suffix(&copy_btn1);
        email_group.add(&email_row1);

        info_box.append(&email_group);
        info_box.append(&detail_extra);
        info_scroll.set_child(Some(&info_box));
        detail_box.append(&info_scroll);

        // The detail pane intentionally describes exactly one contact, while
        // the surrounding actions retain Evolution's 0/1/n selection model.
        // Selection changes rather than row-selected are used because GTK
        // emits row-selected for individual transitions within a multi-select.
        let selected_contact_for_selection = Rc::clone(&selected_contact);
        let stored_contacts_for_selection = Rc::clone(&stored_contacts);
        let page_weak_for_selection = self.downgrade();
        let email_btn_for_selection = email_btn.clone();
        let export_btn_for_selection = export_btn.clone();
        let print_btn_for_selection = print_btn.clone();
        let copy_to_book_btn_for_selection = copy_to_book_btn.clone();
        let move_to_book_btn_for_selection = move_to_book_btn.clone();
        let edit_btn_for_selection = edit_btn.clone();
        let merge_btn_for_selection = merge_btn.clone();
        let delete_btn_for_selection = delete_btn.clone();
        let copy_btn_for_selection = copy_btn1.clone();
        let detail_avatar_for_selection = detail_avatar.clone();
        let detail_name_for_selection = detail_name.clone();
        let detail_title_for_selection = detail_title.clone();
        let detail_extra_for_selection = detail_extra.clone();
        let email_row_for_selection = email_row1.clone();
        contact_list.connect_selected_rows_changed(move |list| {
            let Some(page) = page_weak_for_selection.upgrade() else {
                return;
            };
            if page.imp().is_syncing_selection.get() {
                return;
            }
            page.imp().is_syncing_selection.set(true);
            sync_contact_selection(
                &page,
                list,
                &stored_contacts_for_selection,
                &selected_contact_for_selection,
                &detail_avatar_for_selection,
                &detail_name_for_selection,
                &detail_title_for_selection,
                &detail_extra_for_selection,
                &email_row_for_selection,
                &email_btn_for_selection,
                &export_btn_for_selection,
                &print_btn_for_selection,
                &edit_btn_for_selection,
                &merge_btn_for_selection,
                &delete_btn_for_selection,
                &copy_btn_for_selection,
                &copy_to_book_btn_for_selection,
                &move_to_book_btn_for_selection,
            );
            page.imp().is_syncing_selection.set(false);
            if page.imp().is_mobile.get() && !list.selected_rows().is_empty() {
                page.imp().is_detail_shown.set(true);
                page.sync_contacts_layout_visibility();
                if let Some(root) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                    root.sync_back_button();
                }
            }
        });

        sync_contact_selection(
            self,
            &contact_list,
            &stored_contacts,
            &selected_contact,
            &detail_avatar,
            &detail_name,
            &detail_title,
            &detail_extra,
            &email_row1,
            &email_btn,
            &export_btn,
            &print_btn,
            &edit_btn,
            &merge_btn,
            &delete_btn,
            &copy_btn1,
            &copy_to_book_btn,
            &move_to_book_btn,
        );

        let edit_btn_for_activate = edit_btn.clone();
        contact_list.connect_row_activated(move |_, _| {
            edit_btn_for_activate.activate();
        });

        let click_for_mobile_contact = gtk::GestureClick::new();
        let page_for_contact_click = self.downgrade();
        click_for_mobile_contact.connect_released(move |_, _, _, _| {
            if let Some(page) = page_for_contact_click.upgrade() {
                if page.imp().is_mobile.get() && page.imp().selected_identity.borrow().is_some() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_contacts_layout_visibility();
                    if let Some(root) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                        root.sync_back_button();
                    }
                }
            }
        });
        contact_list.add_controller(click_for_mobile_contact);
        if let Some(flow) = self.imp().contact_cards_flow.borrow().as_ref() {
            let edit_btn_for_cards = edit_btn.clone();
            flow.connect_child_activated(move |_, _| {
                edit_btn_for_cards.activate();
            });
        }

        if let Some((cancel, receiver)) = EBookService::new().start_change_monitor() {
            *self.imp().ebook_monitor_cancel.borrow_mut() = Some(cancel);
            let contact_list_for_monitor = contact_list.clone();
            let stored_contacts_for_monitor = Rc::clone(&stored_contacts);
            let empty_state_for_monitor = empty_state.clone();
            let selected_contact_for_monitor = Rc::clone(&selected_contact);
            let detail_avatar_for_monitor = detail_avatar.clone();
            let detail_name_for_monitor = detail_name.clone();
            let detail_title_for_monitor = detail_title.clone();
            let detail_extra_for_monitor = detail_extra.clone();
            let email_row_for_monitor = email_row1.clone();
            let category_model_for_monitor = category_model.clone();
            let category_selector_for_monitor = category_selector.clone();
            let selected_category_for_monitor = Rc::clone(&selected_category);
            let search_state_for_monitor = Rc::clone(&search_state);
            let page_weak_for_monitor = self.downgrade();
            let email_btn_for_monitor = email_btn.clone();
            let export_btn_for_monitor = export_btn.clone();
            let print_btn_for_monitor = print_btn.clone();
            let edit_btn_for_monitor = edit_btn.clone();
            let merge_btn_for_monitor = merge_btn.clone();
            let delete_btn_for_monitor = delete_btn.clone();
            let copy_btn_for_monitor = copy_btn1.clone();
            let copy_to_book_btn_for_monitor = copy_to_book_btn.clone();
            let move_to_book_btn_for_monitor = move_to_book_btn.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    let Some(page) = page_weak_for_monitor.upgrade() else {
                        break;
                    };
                    if page.imp().disposed.get() {
                        break;
                    }
                    match event {
                        EBookSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "EBook signal monitor started with source warnings"
                                );
                            }
                        }
                        EBookSyncEvent::Change(change) => match change.kind {
                            EBookChangeKind::Removed => {
                                let removed = remove_contact_row(
                                    &contact_list_for_monitor,
                                    Rc::clone(&stored_contacts_for_monitor),
                                    &change.source_uid,
                                     &change.uid,
                                );
                                if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                                    remove_contact_card(flow, &change.source_uid, &change.uid);
                                }
                                if removed
                                    && selected_contact_for_monitor.borrow().as_ref().is_some_and(
                                        |contact| {
                                            contact.source_uid == change.source_uid
                                                && contact.uid == change.uid
                                        },
                                    )
                                {
                                    *selected_contact_for_monitor.borrow_mut() = None;
                                    detail_avatar_for_monitor.set_text(Some(""));
                                    detail_avatar_for_monitor
                                        .set_custom_image(None::<&gdk::Texture>);
                                    detail_name_for_monitor.set_label("Kein Kontakt ausgewählt");
                                    detail_title_for_monitor.set_label(
                                        "Adressbuchdaten werden nach der Synchronisation angezeigt",
                                    );
                                    detail_extra_for_monitor
                                        .set_label("Keine weiteren Kontaktdaten");
                                    email_row_for_monitor.set_title("Keine E-Mail-Adresse");
                                    email_row_for_monitor.set_subtitle("Kein Kontakt ausgewählt");
                                    page.imp().selected_identity.replace(None);
                                    contact_list_for_monitor.unselect_all();
                                    if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                                        flow.unselect_all();
                                    }
                                }
                            }
                            EBookChangeKind::Added | EBookChangeKind::Modified => {
                                if let Some(record) = parse_change_record(&change) {
                                    let was_selected = selected_contact_for_monitor
                                        .borrow()
                                        .as_ref()
                                        .is_some_and(|selected| {
                                            selected.source_uid == record.source_uid
                                                && selected.uid == record.uid
                                        });
                                    upsert_contact_row(
                                        &contact_list_for_monitor,
                                        Rc::clone(&stored_contacts_for_monitor),
                                        record.clone(),
                                    );
                                    if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                                        upsert_contact_card(flow, &record);
                                    }
                                    if was_selected {
                                        // Re-select after replacing the row.  This keeps the
                                        // detail pane and keyboard navigation attached to the
                                        // same source UID + contact UID instead of a stale row.
                                        select_contact_row(
                                            &contact_list_for_monitor,
                                            &record.source_uid,
                                            &record.uid,
                                        );
                                        if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                                            select_contact_card(flow, &record.source_uid, &record.uid);
                                        }
                                        *selected_contact_for_monitor.borrow_mut() =
                                            Some(record.clone());
                                        update_contact_detail(
                                            &record,
                                            &detail_avatar_for_monitor,
                                            &detail_name_for_monitor,
                                            &detail_title_for_monitor,
                                            &detail_extra_for_monitor,
                                            &email_row_for_monitor,
                                        );
                                    }
                                }
                            }
                        },
                        EBookSyncEvent::Error(error) => {
                            tracing::warn!(%error, "EBook signal monitor stopped");
                            break;
                        }
                    }
                    sync_contact_selection(
                        &page,
                        &contact_list_for_monitor,
                        &stored_contacts_for_monitor,
                        &selected_contact_for_monitor,
                        &detail_avatar_for_monitor,
                        &detail_name_for_monitor,
                        &detail_title_for_monitor,
                        &detail_extra_for_monitor,
                        &email_row_for_monitor,
                        &email_btn_for_monitor,
                        &export_btn_for_monitor,
                        &print_btn_for_monitor,
                        &edit_btn_for_monitor,
                        &merge_btn_for_monitor,
                        &delete_btn_for_monitor,
                        &copy_btn_for_monitor,
                        &copy_to_book_btn_for_monitor,
                        &move_to_book_btn_for_monitor,
                    );
                    empty_state_for_monitor
                        .set_visible(stored_contacts_for_monitor.borrow().is_empty());
                    refresh_contact_category_model(
                        &category_model_for_monitor,
                        &category_selector_for_monitor,
                        &stored_contacts_for_monitor,
                        &selected_category_for_monitor,
                    );
                    start_contact_search(
                        page_weak_for_monitor.clone(),
                        search_state_for_monitor.active_query(),
                        Rc::clone(&search_state_for_monitor),
                        Rc::clone(&stored_contacts_for_monitor),
                    );
                }
            });
        }

        list_container.set_size_request(280, -1);
        detail_box.set_size_request(340, -1);
        detail_box.set_vexpand(true);
        detail_box.set_hexpand(true);

        let preview_paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .wide_handle(false)
            .vexpand(true)
            .hexpand(true)
            .build();
        preview_paned.set_start_child(Some(&list_container));
        preview_paned.set_end_child(Some(&detail_box));
        preview_paned.set_resize_start_child(false);
        preview_paned.set_resize_end_child(true);
        preview_paned.set_shrink_start_child(false);
        preview_paned.set_shrink_end_child(false);

        let vpane_pos = AppSettings::try_new()
            .map(|s| s.contact_vpane_position())
            .unwrap_or(380)
            .clamp(260, 900);
        preview_paned.set_position(vpane_pos);
        if let Some(settings) = AppSettings::try_new() {
            let settings_for_notify = settings.clone();
            preview_paned.connect_position_notify(move |paned| {
                let pos = paned.position();
                if (260..=900).contains(&pos) && settings_for_notify.contact_vpane_position() != pos {
                    settings_for_notify.set_contact_vpane_position(pos);
                }
            });

            let preview_weak = preview_paned.downgrade();
            settings.connect_changed(Some("contact-vpane-position"), move |settings, _| {
                if let Some(paned) = preview_weak.upgrade() {
                    let pos = settings.int("contact-vpane-position").clamp(260, 900);
                    if (paned.position() - pos).abs() > 1 {
                        paned.set_position(pos);
                    }
                }
            });
        }
        self.imp()
            .contact_preview_paned
            .replace(Some(preview_paned.clone()));

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .vexpand(true)
            .hexpand(true)
            .build();
        content_box.append(&contacts_toolbar_scroll);
        content_box.append(&contacts_search_bar);
        content_box.append(&preview_paned);

        paned.set_end_child(Some(&content_box));
        paned.set_resize_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);

        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(260)
            .clamp(80, 1500);
        paned.set_position(sidebar_position);
        if let Some(settings) = AppSettings::try_new() {
            let settings_for_notify = settings.clone();
            paned.connect_position_notify(move |paned| {
                if let Some(start) = paned.start_child() {
                    if !start.is_visible() {
                        return;
                    }
                }
                let position = paned.position();
                if (80..=1500).contains(&position) {
                    if settings_for_notify.mail_sidebar_position() != position {
                        settings_for_notify.set_mail_sidebar_position(position);
                    }
                    if settings_for_notify.contact_hpane_position() != position {
                        settings_for_notify.set_contact_hpane_position(position);
                    }
                }
            });

            let paned_weak = paned.downgrade();
            settings.connect_changed(Some("mail-sidebar-position"), move |settings, _| {
                if let Some(paned) = paned_weak.upgrade() {
                    let pos = settings.int("mail-sidebar-position").clamp(80, 1500);
                    if (paned.position() - pos).abs() > 1 {
                        paned.set_position(pos);
                    }
                }
            });
        }

        if let Some(settings) = AppSettings::try_new() {
            let initial_visible = settings.contact_preview_visible();
            detail_box.set_visible(initial_visible);
            preview_toggle.set_active(initial_visible);

            let page_weak_for_toolbar = self.downgrade();
            settings.connect_changed(Some("contact-show-toolbar"), move |settings, _| {
                if let Some(page) = page_weak_for_toolbar.upgrade() {
                    if !page.imp().is_mobile.get() {
                        if let Some(toolbar) = page.imp().toolbar.borrow().as_ref() {
                            toolbar.set_visible(settings.boolean("contact-show-toolbar"));
                        }
                    }
                }
            });


            let detail_box_for_settings = detail_box.clone();
            let preview_toggle_for_settings = preview_toggle.clone();
            settings.connect_changed(Some("contact-preview-visible"), move |settings, _| {
                let visible = settings.boolean("contact-preview-visible");
                detail_box_for_settings.set_visible(visible);
                if preview_toggle_for_settings.is_active() != visible {
                    preview_toggle_for_settings.set_active(visible);
                }
            });

            let detail_extra_for_home = detail_extra.clone();
            let selected_for_home = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-preview-home-before-work"), move |_, _| {
                if let Some(record) = selected_for_home.borrow().as_ref() {
                    detail_extra_for_home.set_label(&contact_detail_summary(&record.contact));
                }
            });

            let detail_extra_for_maps = detail_extra.clone();
            let selected_for_maps = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-preview-show-maps"), move |_, _| {
                if let Some(record) = selected_for_maps.borrow().as_ref() {
                    detail_extra_for_maps.set_label(&contact_detail_summary(&record.contact));
                }
            });

            let detail_extra_for_formatting = detail_extra.clone();
            let selected_for_formatting = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-address-formatting"), move |_, _| {
                if let Some(record) = selected_for_formatting.borrow().as_ref() {
                    detail_extra_for_formatting.set_label(&contact_detail_summary(&record.contact));
                }
            });
        }

        #[cfg(all(target_os = "macos", macos_bridge))]
        let macos_banner = {
            let banner = adw::Banner::builder()
                .title("Apple Kontakte verbinden: Möchten Sie Ihre macOS Kontakte in Evolution anzeigen?")
                .button_label("Apple Kontakte verbinden")
                .revealed(false)
                .build();
            let page_weak = self.downgrade();
            let banner_clone = banner.clone();
            banner.connect_button_clicked(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                let srv = crate::services::MacosIntegrationService::new();
                let status = srv.check_permission(crate::services::MacosPermissionKind::Contacts);
                match status {
                    crate::services::MacosPermissionStatus::NotDetermined => {
                        let page_weak_cb = page.downgrade();
                        srv.request_permission(crate::services::MacosPermissionKind::Contacts, move |granted| {
                            if let Some(page) = page_weak_cb.upgrade() {
                                if granted {
                                    let (sender, receiver) = async_channel::bounded(1);
                                    std::thread::spawn(move || {
                                        let contact_store = crate::services::ContactStoreService::new();
                                        let macos = crate::services::MacosIntegrationService::new();
                                        let _ = macos.sync_apple_contacts_to_store(&contact_store);
                                        let _ = sender.send_blocking(());
                                    });
                                    let page_weak_sync = page.downgrade();
                                    glib::spawn_future_local(async move {
                                        if receiver.recv().await.is_ok() {
                                            if let Some(page) = page_weak_sync.upgrade() {
                                                page.reload_contacts();
                                                page.reload_address_books();
                                                page.show_toast("Apple Kontakte erfolgreich verbunden!");
                                                page.update_macos_permission_banner();
                                            }
                                        }
                                    });
                                } else {
                                    page.update_macos_permission_banner();
                                }
                            }
                        });
                    }
                    crate::services::MacosPermissionStatus::Denied => {
                        srv.open_privacy_settings(crate::services::MacosPermissionKind::Contacts);
                    }
                    _ => {
                        banner_clone.set_revealed(false);
                    }
                }
            });
            self.imp().macos_banner.replace(Some(banner.clone()));
            banner
        };

        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        #[cfg(all(target_os = "macos", macos_bridge))]
        root_box.append(&macos_banner);
        root_box.append(&paned);
        root_box.set_parent(self);

        #[cfg(all(target_os = "macos", macos_bridge))]
        {
            self.update_macos_permission_banner();
            let srv = crate::services::MacosIntegrationService::new();
            let perm = srv.check_permission(crate::services::MacosPermissionKind::Contacts);
            if perm == crate::services::MacosPermissionStatus::NotDetermined {
                let page_weak = self.downgrade();
                srv.request_permission(crate::services::MacosPermissionKind::Contacts, move |granted| {
                    if let Some(page) = page_weak.upgrade() {
                        if granted {
                            let (sender, receiver) = async_channel::bounded(1);
                            std::thread::spawn(move || {
                                let contact_store = crate::services::ContactStoreService::new();
                                let macos = crate::services::MacosIntegrationService::new();
                                let _ = macos.sync_apple_contacts_to_store(&contact_store);
                                let _ = sender.send_blocking(());
                            });
                            let page_weak_sync = page.downgrade();
                            glib::spawn_future_local(async move {
                                if receiver.recv().await.is_ok() {
                                    if let Some(page) = page_weak_sync.upgrade() {
                                        page.reload_contacts();
                                        page.reload_address_books();
                                        page.show_toast("Apple Kontakte erfolgreich verbunden!");
                                        page.update_macos_permission_banner();
                                    }
                                }
                            });
                        } else {
                            page.update_macos_permission_banner();
                        }
                    }
                });
            } else if perm == crate::services::MacosPermissionStatus::Authorized {
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let contact_store = crate::services::ContactStoreService::new();
                    let macos = crate::services::MacosIntegrationService::new();
                    let _ = macos.sync_apple_contacts_to_store(&contact_store);
                    let _ = sender.send_blocking(());
                });
                let page_weak_sync = self.downgrade();
                glib::spawn_future_local(async move {
                    if receiver.recv().await.is_ok() {
                        if let Some(page) = page_weak_sync.upgrade() {
                            page.reload_contacts();
                            page.reload_address_books();
                        }
                    }
                });
            }
        }
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Contact page result");
    }
}

fn refresh_contacts(
    contact_list: gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    empty_state: gtk::Label,
    selected_contact: Rc<RefCell<Option<EBookContactRecord>>>,
    category_model: gtk::StringList,
    category_selector: gtk::DropDown,
    selected_category: Rc<RefCell<String>>,
    refresh_generation: Rc<Cell<u64>>,
    page_weak: glib::WeakRef<ContactsPage>,
    search_state: Rc<ContactPageSearchState>,
) {
    let generation = refresh_generation.get().wrapping_add(1);
    refresh_generation.set(generation);
    let selected_identity = selected_contact
        .borrow()
        .as_ref()
        .map(|contact| (contact.source_uid.clone(), contact.uid.clone()));
    while let Some(row) = contact_list.row_at_index(0) {
        contact_list.remove(&row);
    }
    if let Some(page) = page_weak.upgrade() {
        if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
            while let Some(child) = flow.first_child() {
                flow.remove(&child);
            }
        }
    }
    stored_contacts.borrow_mut().clear();
    search_state.visible_identities.borrow_mut().clear();
    *selected_contact.borrow_mut() = None;
    empty_state.set_label("Kontakte werden geladen…");
    empty_state.set_visible(true);
    if let Some(page) = page_weak.upgrade() {
        if page.imp().disposed.get() {
            return;
        }
        page.imp().selected_identity.replace(None);
        let request = search_state.generations.begin();
        page.imp()
            .contact_search_cancel
            .replace(Some(request.cancellation()));
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.start();
            spinner.set_visible(true);
        }
    } else {
        return;
    }

    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = load_contacts_prefer_eds();
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Contact refresh result was dropped");
        }
    });
    glib::spawn_future_local(async move {
        let is_current = || refresh_generation.get() == generation;
        let page_is_alive = || {
            page_weak
                .upgrade()
                .is_some_and(|page| !page.imp().disposed.get())
        };
        match receiver.recv().await {
            Ok(result) => {
                if !is_current() || !page_is_alive() {
                    return;
                }
                let warning = result.warning.as_deref();
                if let Some(warning) = warning {
                    tracing::warn!(%warning, "Contact refresh completed with an EBook warning");
                }
                if result.contacts.is_empty() {
                    if let Some(page) = page_weak.upgrade() {
                        page.update_drawer_total(0);
                    }
                    empty_state.set_label(&contact_load_status_message(result.origin, 0, warning));
                    empty_state.set_visible(true);
                    refresh_contact_category_model(
                        &category_model,
                        &category_selector,
                        &stored_contacts,
                        &selected_category,
                    );
                    start_contact_search(
                        page_weak.clone(),
                        search_state.active_query(),
                        Rc::clone(&search_state),
                        Rc::clone(&stored_contacts),
                    );
                    return;
                }
                let contact_count = result.contacts.len();
                if let Some(page) = page_weak.upgrade() {
                    page.update_drawer_total(contact_count);
                }
                empty_state.set_label(&contact_load_status_message(
                    result.origin,
                    contact_count,
                    warning,
                ));
                // A successful clean load should not permanently consume
                // sidebar height. Keep the state visible only when it carries
                // an empty/error/warning message the user needs to see.
                empty_state.set_visible(contact_status_needs_visibility(contact_count, warning));
                let contacts = result.contacts;
                let flow_opt = page_weak
                    .upgrade()
                    .and_then(|p| p.imp().contact_cards_flow.borrow().clone());
                let mut stored_ref = stored_contacts.borrow_mut();
                for record in &contacts {
                    append_contact_row(&contact_list, record);
                    if let Some(flow) = flow_opt.as_ref() {
                        append_contact_card(flow, record);
                    }
                    stored_ref.push(record.clone());
                }
                drop(stored_ref);
                mark_duplicate_rows(&contact_list, &stored_contacts);
                refresh_contact_category_model(
                    &category_model,
                    &category_selector,
                    &stored_contacts,
                    &selected_category,
                );
                if let Some((source_uid, uid)) = selected_identity.as_ref() {
                    select_contact_row(&contact_list, source_uid, uid);
                    if let Some(flow) = page_weak
                        .upgrade()
                        .and_then(|p| p.imp().contact_cards_flow.borrow().clone())
                    {
                        select_contact_card(&flow, source_uid, uid);
                    }
                }
                start_contact_search(
                    page_weak.clone(),
                    search_state.active_query(),
                    Rc::clone(&search_state),
                    Rc::clone(&stored_contacts),
                );
            }
            Err(error) => {
                if !page_is_alive() {
                    return;
                }
                empty_state.set_label(&format!("Kontakte konnten nicht geladen werden: {error}"));
                refresh_contact_category_model(
                    &category_model,
                    &category_selector,
                    &stored_contacts,
                    &selected_category,
                );
                if let Some(page) = page_weak.upgrade() {
                    if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                        spinner.stop();
                        spinner.set_visible(false);
                    }
                }
            }
        }
    });
}

fn contact_source_label(display_name: &str, source_uid: &str) -> String {
    let display_name = display_name.trim();
    if !display_name.is_empty() {
        return display_name.to_string();
    }
    if source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID {
        "Lokales Adressbuch".to_string()
    } else {
        "Adressbuch".to_string()
    }
}

fn confirmed_contact_uid(backend_uid: &str, requested_uid: &str) -> String {
    if backend_uid.trim().is_empty() {
        requested_uid.to_string()
    } else {
        backend_uid.to_string()
    }
}

fn contact_origin_label(origin: EBookDataOrigin) -> &'static str {
    match origin {
        EBookDataOrigin::Eds => "synchronisiertem Adressbuch",
        EBookDataOrigin::Graph => "externem Adressbuch",
        EBookDataOrigin::Local => "lokalem Adressbuch",
    }
}

fn contact_load_status_message(
    origin: EBookDataOrigin,
    contact_count: usize,
    warning: Option<&str>,
) -> String {
    let message = if contact_count == 0 {
        match origin {
            EBookDataOrigin::Eds => "Keine synchronisierten Kontakte geladen".to_string(),
            EBookDataOrigin::Graph => {
                "Keine Kontakte aus dem externen Adressbuch geladen".to_string()
            }
            EBookDataOrigin::Local => {
                "Keine Kontakte aus dem lokalen Adressbuch geladen".to_string()
            }
        }
    } else {
        format!(
            "{contact_count} Kontakt(e) aus {} geladen.",
            contact_origin_label(origin)
        )
    };
    match warning.map(str::trim).filter(|warning| !warning.is_empty()) {
        Some(warning) => format!("{message} Hinweis: {warning}"),
        None => message,
    }
}

fn contact_status_needs_visibility(contact_count: usize, warning: Option<&str>) -> bool {
    contact_count == 0
        || warning
            .map(str::trim)
            .is_some_and(|warning| !warning.is_empty())
}

fn select_contact_row(contact_list: &gtk::ListBox, source_uid: &str, uid: &str) -> bool {
    select_contact_row_ext(contact_list, source_uid, uid, true, false)
}

fn select_contact_row_ext(
    contact_list: &gtk::ListBox,
    source_uid: &str,
    uid: &str,
    unselect_others: bool,
    toggle: bool,
) -> bool {
    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            row.data::<String>("contact-source-uid")
                .is_some_and(|value| value.as_ref() == source_uid)
                && row
                    .data::<String>("contact-id")
                    .is_some_and(|value| value.as_ref() == uid)
        };
        if matches {
            if unselect_others {
                contact_list.unselect_all();
                contact_list.select_row(Some(&row));
            } else if toggle {
                if row.is_selected() {
                    contact_list.unselect_row(&row);
                } else {
                    contact_list.select_row(Some(&row));
                }
            } else {
                contact_list.select_row(Some(&row));
            }
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContactActionState {
    email: bool,
    export: bool,
    edit: bool,
    merge: bool,
    delete: bool,
    copy: bool,
    print: bool,
    transfer_copy: bool,
    transfer_move: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContactSelectionActionState {
    selection_count: usize,
    email: bool,
    export: bool,
    edit: bool,
    merge: bool,
    delete: bool,
    copy: bool,
    print: bool,
    transfer_copy: bool,
    transfer_move: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContactTransferKind {
    Copy,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactTransferItem {
    pub(crate) identity: ContactIdentity,
    pub(crate) name: String,
    pub(crate) source_uid: String,
    pub(crate) source_display_name: String,
    pub(crate) vcard: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactTransferPlan {
    pub(crate) items: Vec<ContactTransferItem>,
    pub(crate) kind: ContactTransferKind,
    pub(crate) destination_uid: String,
    pub(crate) destination_display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactTransferWorkerResult {
    pub(crate) kind: ContactTransferKind,
    pub(crate) destination_uid: String,
    pub(crate) destination_display_name: String,
    pub(crate) created: Vec<EBookContactRecord>,
    pub(crate) removed_from_source: Vec<ContactIdentity>,
    pub(crate) failure: Option<(ContactIdentity, String)>,
}

#[derive(Clone)]
struct ContactTransferUiContext {
    contact_list: gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    selected_contact: Rc<RefCell<Option<EBookContactRecord>>>,
    detail_avatar: adw::Avatar,
    detail_name: gtk::Label,
    detail_title: gtk::Label,
    detail_extra: gtk::Label,
    email_btn: gtk::Button,
    export_btn: gtk::Button,
    print_btn: gtk::Button,
    edit_btn: gtk::Button,
    merge_btn: gtk::Button,
    delete_btn: gtk::Button,
    copy_to_book_btn: gtk::Button,
    move_to_book_btn: gtk::Button,
    category_model: gtk::StringList,
    category_selector: gtk::DropDown,
    selected_category: Rc<RefCell<String>>,
    empty_state: gtk::Label,
    search_state: Rc<ContactPageSearchState>,
}

impl ContactTransferUiContext {
    fn update_action_sensitivity(&self) {
        update_contact_action_sensitivity(
            &self.email_btn,
            &self.export_btn,
            &self.edit_btn,
            &self.merge_btn,
            &self.delete_btn,
            Some(&self.print_btn),
            None,
            Some(&self.copy_to_book_btn),
            Some(&self.move_to_book_btn),
            &self.selected_contact,
            &self.stored_contacts,
        );
    }
}

fn prepare_vcard_for_transfer(vcard: &str) -> String {
    let fresh_uid = format!("evolution-gtk4-{}", uuid::Uuid::new_v4());
    let normalized = vcard.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = normalized.lines().map(ToString::to_string).collect();
    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with("UID:") || line.starts_with("UID;") {
            *line = format!("UID:{fresh_uid}");
            replaced = true;
            break;
        }
    }
    if !replaced {
        let insertion = lines
            .iter()
            .position(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
            .map(|index| index + 1)
            .unwrap_or(0);
        lines.insert(insertion, format!("UID:{fresh_uid}"));
    }
    let mut result = lines.join("\r\n");
    if !result.ends_with("\r\n") {
        result.push_str("\r\n");
    }
    result
}

fn plan_contact_transfer(
    selected: &[EBookContactRecord],
    kind: ContactTransferKind,
    destination_uid: &str,
    destination_display_name: &str,
    destination_writable: bool,
) -> Result<ContactTransferPlan, String> {
    if selected.is_empty() {
        return Err("Keine Kontakte ausgewählt".to_string());
    }
    let destination_uid = destination_uid.trim();
    if destination_uid.is_empty() || !destination_writable {
        return Err("Das Zieladressbuch ist nicht schreibfähig".to_string());
    }

    if selected.iter().all(|r| r.source_uid == destination_uid) {
        return Err("Quelle und Ziel sind dasselbe Adressbuch".to_string());
    }

    let mut seen = HashSet::with_capacity(selected.len());
    let mut items = Vec::with_capacity(selected.len());

    for record in selected {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity.clone()) {
            return Err("Ein ausgewählter Kontakt wurde mehrfach aufgelöst".to_string());
        }
        if kind == ContactTransferKind::Move && !contact_source_is_writable(&record.source_uid) {
            return Err(
                "Mindestens ein ausgewählter Kontakt stammt aus einer schreibgeschützten Quelle"
                    .to_string(),
            );
        }
        let name = if record.contact.fn_name.trim().is_empty() {
            "Unbenannter Kontakt".to_string()
        } else {
            record.contact.fn_name.trim().to_string()
        };
        let source_display_name =
            contact_source_label(&record.source_display_name, &record.source_uid);
        let vcard = record.vcard.trim();
        if vcard.is_empty() {
            return Err(format!(
                "Kontakt „{name}“ enthält keine gültigen vCard-Daten für die Übertragung"
            ));
        }

        items.push(ContactTransferItem {
            identity,
            name,
            source_uid: record.source_uid.clone(),
            source_display_name,
            vcard: vcard.to_string(),
        });
    }

    Ok(ContactTransferPlan {
        items,
        kind,
        destination_uid: destination_uid.to_string(),
        destination_display_name: destination_display_name.trim().to_string(),
    })
}

fn reduce_contact_transfer_result(
    stored_contacts: &[EBookContactRecord],
    removed: &[ContactIdentity],
    created: &[EBookContactRecord],
) -> Vec<EBookContactRecord> {
    let mut updated: Vec<EBookContactRecord> = stored_contacts
        .iter()
        .filter(|record| {
            !removed.iter().any(|identity| {
                identity.source_uid == record.source_uid && identity.uid == record.uid
            })
        })
        .cloned()
        .collect();

    for record in created {
        if !updated
            .iter()
            .any(|r| r.source_uid == record.source_uid && r.uid == record.uid)
        {
            updated.push(record.clone());
        }
    }
    updated
}

fn available_writable_address_books(
    book_model: &gtk::StringList,
    book_source_uids: &[String],
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (index, uid) in book_source_uids.iter().enumerate() {
        if index == 0 || uid.trim().is_empty() {
            continue;
        }
        if contact_source_is_writable(uid) {
            let label = book_model
                .string(index as u32)
                .map(|s| s.to_string())
                .unwrap_or_else(|| uid.clone());
            result.push((uid.clone(), label));
        }
    }
    if result.is_empty()
        && crate::services::contact_store_service::ContactStoreService::new().is_writable()
    {
        result.push((
            LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string(),
            "Lokales Adressbuch".to_string(),
        ));
    }
    result
}

fn run_contact_transfer_dialog(
    parent: &impl glib::object::IsA<gtk::Widget>,
    page: &ContactsPage,
    kind: ContactTransferKind,
    selected_records: Vec<EBookContactRecord>,
    book_model: &gtk::StringList,
    book_source_uids: &[String],
    ctx: ContactTransferUiContext,
) {
    let available = available_writable_address_books(book_model, book_source_uids);
    let common_source = if selected_records
        .iter()
        .all(|r| r.source_uid == selected_records[0].source_uid)
    {
        Some(selected_records[0].source_uid.as_str())
    } else {
        None
    };

    let eligible: Vec<(String, String)> = available
        .into_iter()
        .filter(|(uid, _)| common_source.map_or(true, |common| uid != common))
        .collect();

    if eligible.is_empty() {
        page.show_toast("Kein anderes beschreibbares Zieladressbuch verfügbar");
        return;
    }

    let dest_names: Vec<&str> = eligible.iter().map(|(_, name)| name.as_str()).collect();
    let dest_model = gtk::StringList::new(&dest_names);

    let count = selected_records.len();
    let (title, description, confirm_label, appearance) = match kind {
        ContactTransferKind::Copy => {
            let title = if count == 1 {
                "Kontakt kopieren".to_string()
            } else {
                format!("{count} Kontakte kopieren")
            };
            let description = if count == 1 {
                "Wählen Sie das Zieladressbuch aus, in das der Kontakt kopiert werden soll:"
                    .to_string()
            } else {
                "Wählen Sie das Zieladressbuch aus, in das die Kontakte kopiert werden sollen:"
                    .to_string()
            };
            (
                title,
                description,
                "Kopieren",
                adw::ResponseAppearance::Suggested,
            )
        }
        ContactTransferKind::Move => {
            let title = if count == 1 {
                "Kontakt verschieben".to_string()
            } else {
                format!("{count} Kontakte verschieben")
            };
            let description = if count == 1 {
                "Der Kontakt wird im Zieladressbuch neu angelegt und anschließend aus dem bisherigen Adressbuch entfernt.".to_string()
            } else {
                "Die ausgewählten Kontakte werden im Zieladressbuch neu angelegt und anschließend aus ihren bisherigen Adressbüchern entfernt.".to_string()
            };
            (
                title,
                description,
                "Verschieben",
                adw::ResponseAppearance::Suggested,
            )
        }
    };

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    let dropdown = gtk::DropDown::new(Some(dest_model), None::<&gtk::Expression>);
    dropdown.set_hexpand(true);
    content_box.append(&dropdown);

    let dialog = adw::AlertDialog::new(Some(&title), Some(&description));
    dialog.set_extra_child(Some(&content_box));
    dialog.add_response("cancel", "Abbrechen");
    dialog.add_response("confirm", confirm_label);
    dialog.set_response_appearance("confirm", appearance);
    dialog.set_default_response(Some("confirm"));
    dialog.set_close_response("cancel");

    let page_weak = page.downgrade();
    dialog.connect_response(None, move |_, response| {
        if response != "confirm" {
            return;
        }
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        let selected_idx = dropdown.selected() as usize;
        let Some((dest_uid, dest_display_name)) = eligible.get(selected_idx).cloned() else {
            page.show_toast("Ungültige Adressbuchauswahl");
            return;
        };

        let plan = match plan_contact_transfer(
            &selected_records,
            kind,
            &dest_uid,
            &dest_display_name,
            contact_source_is_writable(&dest_uid),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                page.show_toast(&error);
                return;
            }
        };

        execute_contact_transfer_worker(plan, page.downgrade(), ctx.clone());
    });

    dialog.present(Some(parent));
}

fn execute_contact_transfer_worker(
    plan: ContactTransferPlan,
    page_weak: glib::WeakRef<ContactsPage>,
    ctx: ContactTransferUiContext,
) {
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let service = EBookService::new();
        let mut result = ContactTransferWorkerResult {
            kind: plan.kind,
            destination_uid: plan.destination_uid.clone(),
            destination_display_name: plan.destination_display_name.clone(),
            created: Vec::with_capacity(plan.items.len()),
            removed_from_source: Vec::with_capacity(if plan.kind == ContactTransferKind::Move {
                plan.items.len()
            } else {
                0
            }),
            failure: None,
        };

        for item in plan.items {
            if !contact_source_is_writable(&plan.destination_uid) {
                result.failure = Some((
                    item.identity,
                    "Das Zieladressbuch ist nicht mehr schreibfähig".to_string(),
                ));
                break;
            }

            if plan.kind == ContactTransferKind::Move
                && !contact_source_is_writable(&item.source_uid)
            {
                result.failure = Some((
                    item.identity,
                    "Die Quelladresse ist unmittelbar vor dem Verschieben nicht mehr schreibfähig"
                        .to_string(),
                ));
                break;
            }

            let transfer_vcard = prepare_vcard_for_transfer(&item.vcard);
            match service.create_contact(&plan.destination_uid, &transfer_vcard) {
                Ok(mutation) => {
                    let created_uid = confirmed_contact_uid(&mutation.uid, &item.identity.uid);
                    let vcard_content = mutation.vcard.unwrap_or(transfer_vcard);
                    let parsed_contact = VCardContact::parse_vcard_string(&vcard_content)
                        .unwrap_or_else(|| VCardContact::new(&item.name, "", "", ""));
                    result.created.push(EBookContactRecord {
                        source_uid: plan.destination_uid.clone(),
                        source_display_name: plan.destination_display_name.clone(),
                        uid: created_uid,
                        vcard: vcard_content,
                        contact: parsed_contact,
                    });
                }
                Err(error) => {
                    result.failure = Some((
                        item.identity,
                        format!("Erstellen im Zieladressbuch fehlgeschlagen: {error}"),
                    ));
                    break;
                }
            }

            if plan.kind == ContactTransferKind::Move {
                match service.remove_contact(&item.identity.source_uid, &item.identity.uid) {
                    Ok(confirmed) => {
                        let confirmed_uid =
                            confirmed_contact_uid(&confirmed.uid, &item.identity.uid);
                        if confirmed_uid == item.identity.uid {
                            result.removed_from_source.push(item.identity);
                        } else {
                            result.failure = Some((
                                item.identity,
                                "Das Backend bestätigte das Löschen eines anderen Kontakts"
                                    .to_string(),
                            ));
                            break;
                        }
                    }
                    Err(error) => {
                        result.failure = Some((
                            item.identity,
                            format!("Entfernen aus dem Quelladressbuch fehlgeschlagen: {error}"),
                        ));
                        break;
                    }
                }
            }
        }

        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Contact transfer result was dropped");
        }
    });

    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let created_count = result.created.len();
        let removed_count = result.removed_from_source.len();

        if created_count > 0 || removed_count > 0 {
            if let Some(page) = page_weak.upgrade() {
                clear_contact_selection(&page, &ctx.contact_list);
            }
            for removed_id in &result.removed_from_source {
                remove_contact_list_row(&ctx.contact_list, &removed_id.source_uid, &removed_id.uid);
            }
            for record in &result.created {
                append_contact_row(&ctx.contact_list, record);
            }
            let updated = reduce_contact_transfer_result(
                &ctx.stored_contacts.borrow(),
                &result.removed_from_source,
                &result.created,
            );
            *ctx.stored_contacts.borrow_mut() = updated;

            let selected_is_removed = ctx
                .selected_contact
                .borrow()
                .as_ref()
                .map(|c| {
                    result
                        .removed_from_source
                        .iter()
                        .any(|r| r.source_uid == c.source_uid && r.uid == c.uid)
                })
                .unwrap_or(false);

            if selected_is_removed {
                *ctx.selected_contact.borrow_mut() = None;
                ctx.detail_avatar.set_text(Some(""));
                ctx.detail_avatar.set_custom_image(None::<&gdk::Texture>);
                ctx.detail_name.set_label("Kein Kontakt ausgewählt");
                ctx.detail_title
                    .set_label("Adressbuchdaten werden nach der Synchronisation angezeigt");
                ctx.detail_extra.set_label("Keine weiteren Kontaktdaten");
            }

            mark_duplicate_rows(&ctx.contact_list, &ctx.stored_contacts);
            ctx.empty_state
                .set_visible(ctx.stored_contacts.borrow().is_empty());
            refresh_contact_category_model(
                &ctx.category_model,
                &ctx.category_selector,
                &ctx.stored_contacts,
                &ctx.selected_category,
            );
            ctx.update_action_sensitivity();
            if let Some(page) = page_weak.upgrade() {
                reapply_active_contact_search(
                    page.downgrade(),
                    Rc::clone(&ctx.search_state),
                    Rc::clone(&ctx.stored_contacts),
                );
            }
        }

        if let Some((failed_id, error)) = result.failure {
            let message = if created_count > 0 {
                format!(
                    "{created_count} Kontakt(e) übertragen; Fehler bei {}: {error}",
                    failed_id.uid
                )
            } else {
                format!("Übertragung fehlgeschlagen: {error}")
            };
            show_toast_for_widget(&ctx.detail_name, &message);
        } else {
            let dest = &result.destination_display_name;
            let message = match result.kind {
                ContactTransferKind::Copy => {
                    if created_count == 1 {
                        format!("1 Kontakt nach „{dest}“ kopiert")
                    } else {
                        format!("{created_count} Kontakte nach „{dest}“ kopiert")
                    }
                }
                ContactTransferKind::Move => {
                    if created_count == 1 {
                        format!("1 Kontakt nach „{dest}“ verschoben")
                    } else {
                        format!("{created_count} Kontakte nach „{dest}“ verschoben")
                    }
                }
            };
            if let Some(page) = page_weak.upgrade() {
                page.show_toast(&message);
            }
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactDeleteResolved {
    pub(crate) identity: ContactIdentity,
    pub(crate) source_display_name: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactDeleteConfirmation {
    pub(crate) title: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactDeletePlan {
    pub(crate) items: Vec<ContactDeleteResolved>,
    pub(crate) confirmation: ContactDeleteConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactDeleteWorkerResult {
    pub(crate) removed: Vec<ContactIdentity>,
    pub(crate) failure: Option<(ContactIdentity, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContactVCardExportPlan {
    initial_name: String,
    payload: Vec<u8>,
    contact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContactVCardExportError {
    NoSelection,
    InvalidVCard { contact_name: String },
}

impl ContactVCardExportError {
    fn user_message(&self) -> String {
        match self {
            Self::NoSelection => "Keine Kontakte ausgewählt".to_string(),
            Self::InvalidVCard { contact_name } => format!(
                "vCard-Export nicht gestartet: „{contact_name}“ enthält keine vollständige vCard"
            ),
        }
    }
}

fn contact_source_is_writable(source_uid: &str) -> bool {
    let source_uid = source_uid.trim();
    if source_uid.is_empty() {
        return false;
    }
    if source_uid == LOCAL_ADDRESS_BOOK_SOURCE_UID {
        return crate::services::contact_store_service::ContactStoreService::new().is_writable();
    }
    // EBookContactRecord does not carry a fabricated read-only flag.  A
    // non-empty source UID is the capability metadata supplied by the EBook
    // layer for an address-book-backed record; imported records have no UID
    // and remain explicitly read-only.
    true
}

fn plan_contact_delete_batch(selected: &[EBookContactRecord]) -> Result<ContactDeletePlan, String> {
    if selected.is_empty() {
        return Err("Keine Kontakte ausgewählt".to_string());
    }

    let mut seen = HashSet::with_capacity(selected.len());
    let mut items = Vec::with_capacity(selected.len());

    for record in selected {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity.clone()) {
            return Err("Ein ausgewählter Kontakt wurde mehrfach aufgelöst".to_string());
        }
        if !contact_source_is_writable(&record.source_uid) {
            return Err(
                "Mindestens ein ausgewählter Kontakt stammt aus einer schreibgeschützten Quelle"
                    .to_string(),
            );
        }
        let name = if record.contact.fn_name.trim().is_empty() {
            "Unbenannter Kontakt".to_string()
        } else {
            record.contact.fn_name.trim().to_string()
        };
        let source_display_name =
            contact_source_label(&record.source_display_name, &record.source_uid);
        items.push(ContactDeleteResolved {
            identity,
            source_display_name,
            name,
        });
    }

    let confirmation = if items.len() == 1 {
        let item = &items[0];
        ContactDeleteConfirmation {
            title: "Kontakt löschen?".to_string(),
            description: format!(
                "Der Kontakt „{}“ wird dauerhaft aus dem Adressbuch „{}“ entfernt.",
                item.name, item.source_display_name
            ),
        }
    } else {
        ContactDeleteConfirmation {
            title: "Kontakte löschen?".to_string(),
            description: format!(
                "Möchten Sie die {} ausgewählten Kontakte wirklich dauerhaft aus ihren Adressbüchern entfernen?",
                items.len()
            ),
        }
    };

    Ok(ContactDeletePlan {
        items,
        confirmation,
    })
}

fn reduce_contact_delete_result(
    stored_contacts: &[EBookContactRecord],
    removed: &[ContactIdentity],
) -> Vec<EBookContactRecord> {
    stored_contacts
        .iter()
        .filter(|record| {
            !removed.iter().any(|identity| {
                identity.source_uid == record.source_uid && identity.uid == record.uid
            })
        })
        .cloned()
        .collect()
}

fn contact_action_state(
    selected: Option<&EBookContactRecord>,
    contacts: &[EBookContactRecord],
) -> ContactActionState {
    let Some(record) = selected else {
        return ContactActionState {
            email: false,
            export: false,
            edit: false,
            merge: false,
            delete: false,
            copy: false,
            print: false,
            transfer_copy: false,
            transfer_move: false,
        };
    };
    let recipient = contact_recipient_text_with_records(&record.contact, contacts);
    let copyable = if record.contact.is_list {
        record.contact.list_show_addresses && !recipient.trim().is_empty()
    } else {
        !contact_email_text(&record.contact).trim().is_empty()
    };
    let writable = contact_source_is_writable(&record.source_uid);
    ContactActionState {
        email: !recipient.trim().is_empty(),
        export: true,
        edit: writable,
        merge: writable && contact_has_mergeable_duplicate(record, contacts),
        delete: writable,
        copy: copyable,
        print: true,
        transfer_copy: true,
        transfer_move: writable,
    }
}

fn context_action_state(
    selected: Option<&EBookContactRecord>,
    contacts: &[EBookContactRecord],
) -> ContactActionState {
    contact_action_state(selected, contacts)
}

fn contact_selection_action_state(
    selected: &[EBookContactRecord],
    contacts: &[EBookContactRecord],
) -> ContactSelectionActionState {
    let selection_count = selected.len();
    let single = selection_count == 1;
    let single_state = single.then(|| contact_action_state(selected.first(), contacts));
    let recipients = selected_contact_recipients(selected, contacts);
    let all_writable = !selected.is_empty()
        && selected
            .iter()
            .all(|record| contact_source_is_writable(&record.source_uid));
    ContactSelectionActionState {
        selection_count,
        email: !recipients.trim().is_empty(),
        // The export button only becomes enabled once the whole selection has
        // been validated as one payload.  This is the fail-closed half of the
        // atomic save contract; the click handler plans it again from the
        // current selection before the file picker is opened.
        export: plan_contact_vcard_export(selected).is_ok(),
        edit: single_state.is_some_and(|state| state.edit),
        merge: single_state.is_some_and(|state| state.merge),
        // Batch deletion is supported across all-writable contacts with a
        // preflight check, confirmation dialog, and stop-on-first-error worker.
        delete: all_writable,
        copy: single_state.is_some_and(|state| state.copy),
        print: !selected.is_empty(),
        transfer_copy: !selected.is_empty(),
        transfer_move: all_writable,
    }
}

fn update_contact_action_sensitivity(
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    print_button: Option<&gtk::Button>,
    copy_button: Option<&gtk::Button>,
    transfer_copy_button: Option<&gtk::Button>,
    transfer_move_button: Option<&gtk::Button>,
    selected: &Rc<RefCell<Option<EBookContactRecord>>>,
    contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let selected = selected.borrow();
    let contacts = contacts.borrow();
    let state = contact_action_state(selected.as_ref(), &contacts);
    apply_contact_action_state(
        state,
        email_button,
        export_button,
        edit_button,
        merge_button,
        delete_button,
        print_button,
        copy_button,
        transfer_copy_button,
        transfer_move_button,
    );
}

fn apply_contact_action_state(
    state: ContactActionState,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    print_button: Option<&gtk::Button>,
    copy_button: Option<&gtk::Button>,
    transfer_copy_button: Option<&gtk::Button>,
    transfer_move_button: Option<&gtk::Button>,
) {
    email_button.set_sensitive(state.email);
    export_button.set_sensitive(state.export);
    edit_button.set_sensitive(state.edit);
    merge_button.set_sensitive(state.merge);
    delete_button.set_sensitive(state.delete);
    if let Some(print_button) = print_button {
        print_button.set_sensitive(state.print);
    }
    if let Some(copy_button) = copy_button {
        copy_button.set_sensitive(state.copy);
    }
    if let Some(transfer_copy_button) = transfer_copy_button {
        transfer_copy_button.set_sensitive(state.transfer_copy);
    }
    if let Some(transfer_move_button) = transfer_move_button {
        transfer_move_button.set_sensitive(state.transfer_move);
    }
}

fn apply_contact_selection_action_state(
    state: ContactSelectionActionState,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    print_button: &gtk::Button,
    copy_button: &gtk::Button,
    transfer_copy_button: &gtk::Button,
    transfer_move_button: &gtk::Button,
) {
    email_button.set_sensitive(state.email);
    export_button.set_sensitive(state.export);
    edit_button.set_sensitive(state.edit);
    merge_button.set_sensitive(state.merge);
    delete_button.set_sensitive(state.delete);
    print_button.set_sensitive(state.print);
    copy_button.set_sensitive(state.copy);
    transfer_copy_button.set_sensitive(state.transfer_copy);
    transfer_move_button.set_sensitive(state.transfer_move);

    let multiple = state.selection_count > 1;
    email_button.set_tooltip_text(Some(if state.email {
        "E-Mail an ausgewählte Kontakte senden"
    } else {
        "Die Auswahl enthält keine E-Mail-Adresse"
    }));
    export_button.set_tooltip_text(Some(if state.export {
        "Ausgewählte Kontakte gemeinsam als vCard exportieren"
    } else if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else {
        "Die Auswahl enthält keine vollständig exportierbare vCard"
    }));
    print_button.set_tooltip_text(Some(if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else if multiple {
        "Ausgewählte Kontakte drucken"
    } else {
        "Ausgewählten Kontakt drucken"
    }));
    transfer_copy_button.set_tooltip_text(Some(if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else if multiple {
        "Ausgewählte Kontakte in anderes Adressbuch kopieren"
    } else {
        "Kontakt in anderes Adressbuch kopieren"
    }));
    transfer_move_button.set_tooltip_text(Some(if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else if !state.transfer_move {
        "Die Auswahl enthält Kontakte aus einer schreibgeschützten Quelle"
    } else if multiple {
        "Ausgewählte Kontakte in anderes Adressbuch verschieben"
    } else {
        "Kontakt in anderes Adressbuch verschieben"
    }));
    edit_button.set_tooltip_text(Some(if multiple {
        "Bearbeiten ist nur für genau einen Kontakt verfügbar"
    } else {
        "Kontakt bearbeiten"
    }));
    merge_button.set_tooltip_text(Some(if multiple {
        "Zusammenführen ist nur für genau einen Kontakt verfügbar"
    } else {
        "Dubletten derselben Quelle zusammenführen"
    }));
    delete_button.set_tooltip_text(Some(if state.selection_count == 0 {
        "Kein Kontakt ausgewählt"
    } else if !state.delete {
        "Die Auswahl enthält Kontakte aus einer schreibgeschützten Quelle"
    } else if multiple {
        "Ausgewählte Kontakte löschen"
    } else {
        "Kontakt löschen"
    }));
}

fn set_contact_filter(contact_list: &gtk::ListBox, search_state: Rc<ContactPageSearchState>) {
    contact_list.set_filter_func(move |row| unsafe {
        let source_uid = row
            .data::<String>("contact-source-uid")
            .map(|value| value.as_ref().clone())
            .unwrap_or_default();
        let uid = row
            .data::<String>("contact-id")
            .map(|value| value.as_ref().clone())
            .unwrap_or_default();
        search_state
            .visible_identities
            .borrow()
            .contains(&ContactIdentity::new(source_uid, uid))
    });
}

fn quick_field_for_index(index: u32) -> ContactSearchField {
    match index {
        1 => ContactSearchField::Name,
        2 => ContactSearchField::Email,
        3 => ContactSearchField::Phone,
        4 => ContactSearchField::Category,
        _ => ContactSearchField::AnyField,
    }
}

fn quick_query(field: ContactSearchField, text: &str) -> ContactSearchQuery {
    if text.trim().is_empty() {
        ContactSearchQuery::all()
    } else if field == ContactSearchField::AnyField {
        ContactSearchQuery::global_search(text)
    } else {
        ContactSearchQuery::contains(field, text)
    }
}

fn current_outer_filters(
    selected_book: &Rc<RefCell<String>>,
    selected_category: &Rc<RefCell<String>>,
    unmatched_category: bool,
    duplicate_only: bool,
) -> crate::services::contact_search_service::ContactOuterFilters {
    let source_uid = selected_book.borrow().trim().to_string();
    let category = selected_category.borrow().trim().to_string();
    crate::services::contact_search_service::ContactOuterFilters {
        source_uid: (!source_uid.is_empty()).then_some(source_uid),
        category: (!category.is_empty()).then_some(category),
        unmatched_category,
        duplicate_only,
    }
}

fn with_contact_outer_filters(
    mut query: ContactSearchQuery,
    selected_book: &Rc<RefCell<String>>,
    selected_category: &Rc<RefCell<String>>,
    unmatched_category: bool,
    duplicate_only: bool,
) -> ContactSearchQuery {
    query.filters = current_outer_filters(
        selected_book,
        selected_category,
        unmatched_category,
        duplicate_only,
    );
    query
}

fn snapshot_records_cancelable(
    records: &[EBookContactRecord],
    cancellation: &AtomicBool,
) -> Option<Vec<ContactSearchSnapshot>> {
    let mut snapshots = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        if index % 16 == 0 && cancellation.load(Ordering::Relaxed) {
            return None;
        }
        snapshots.push(ContactSearchSnapshot::with_source_display_name(
            record.source_uid.clone(),
            record.uid.clone(),
            record.source_display_name.clone(),
            record.contact.clone(),
        ));
    }
    Some(snapshots)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContactSelectionRestore {
    generation: u64,
    identity: ContactIdentity,
}

fn row_contact_identity(row: &gtk::ListBoxRow) -> Option<ContactIdentity> {
    unsafe {
        Some(ContactIdentity::new(
            row.data::<String>("contact-source-uid")?.as_ref().clone(),
            row.data::<String>("contact-id")?.as_ref().clone(),
        ))
    }
}

fn selected_contact_identity(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
) -> Option<ContactIdentity> {
    contact_list
        .selected_row()
        .as_ref()
        .and_then(row_contact_identity)
        .or_else(|| page.imp().selected_identity.borrow().clone())
}

fn selected_contact_records_checked(
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) -> Result<Vec<EBookContactRecord>, String> {
    let identities = contact_list
        .selected_rows()
        .iter()
        .filter_map(row_contact_identity)
        .collect::<Vec<_>>();
    if identities.is_empty() {
        return Err("Keine Kontakte ausgewählt".to_string());
    }

    let records = stored_contacts.borrow();
    let mut selected = Vec::with_capacity(identities.len());
    for identity in identities {
        let matches = records
            .iter()
            .filter(|record| record.source_uid == identity.source_uid && record.uid == identity.uid)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [record] => selected.push((*record).clone()),
            _ => {
                return Err(
                    "Die Kontaktauswahl ist nicht mehr aktuell; bitte neu auswählen".to_string(),
                );
            }
        }
    }
    Ok(selected)
}

fn visible_contact_records_checked(
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
    search_state: &Rc<ContactPageSearchState>,
) -> Result<Vec<EBookContactRecord>, String> {
    let visible_ids = search_state.visible_identities.borrow();
    let records = stored_contacts.borrow();
    let mut visible = Vec::new();

    let mut child = contact_list.first_child();
    while let Some(current) = child {
        let next = current.next_sibling();
        if let Ok(row) = current.downcast::<gtk::ListBoxRow>() {
            if let Some(identity) = row_contact_identity(&row) {
                let is_visible = if visible_ids.is_empty() {
                    true
                } else {
                    visible_ids.contains(&identity)
                };
                if is_visible {
                    let matches = records
                        .iter()
                        .filter(|r| r.source_uid == identity.source_uid && r.uid == identity.uid)
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [record] => visible.push((*record).clone()),
                        _ => {
                            return Err(
                                "Ein sichtbarer Kontakt ist nicht mehr eindeutig verfügbar"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
        }
        child = next;
    }

    if visible.is_empty() {
        return Err("Es sind keine sichtbaren Kontakte zum Drucken vorhanden".to_string());
    }
    Ok(visible)
}

fn selected_contact_recipients(
    selected: &[EBookContactRecord],
    records: &[EBookContactRecord],
) -> String {
    let mut recipients = Vec::new();
    for record in selected {
        for recipient in contact_recipient_text_with_records(&record.contact, records).split(',') {
            push_unique_recipient(&mut recipients, recipient);
        }
    }
    recipients.join(", ")
}

fn plan_contact_vcard_export(
    selected: &[EBookContactRecord],
) -> Result<ContactVCardExportPlan, ContactVCardExportError> {
    if selected.is_empty() {
        return Err(ContactVCardExportError::NoSelection);
    }

    let mut payload = Vec::new();
    for record in selected {
        if VCardContact::parse_vcard_string(&record.vcard).is_none() {
            return Err(ContactVCardExportError::InvalidVCard {
                contact_name: record.contact.fn_name.clone(),
            });
        }
        // Retain every provider-supplied byte except trailing line-breaks;
        // one CRLF is then appended so adjacent cards stay distinct without
        // reserializing vCard 3/4 or extension data through a lossy UI path.
        let vcard = record.vcard.trim_end_matches(['\r', '\n']);
        payload.extend_from_slice(vcard.as_bytes());
        payload.extend_from_slice(b"\r\n");
    }

    Ok(ContactVCardExportPlan {
        initial_name: if selected.len() == 1 {
            "kontakt.vcf".to_string()
        } else {
            "kontakte.vcf".to_string()
        },
        payload,
        contact_count: selected.len(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactExportPlan {
    pub(crate) initial_name: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) contact_count: usize,
    pub(crate) format: ContactExportFormat,
}

pub(crate) fn plan_contact_export(
    selected: &[EBookContactRecord],
    format: ContactExportFormat,
) -> Result<ContactExportPlan, String> {
    if selected.is_empty() {
        return Err("Keine Kontakte ausgewählt".to_string());
    }

    match format {
        ContactExportFormat::VCard => {
            let vcard_plan =
                plan_contact_vcard_export(selected).map_err(|err| err.user_message())?;
            Ok(ContactExportPlan {
                initial_name: vcard_plan.initial_name,
                payload: vcard_plan.payload,
                contact_count: vcard_plan.contact_count,
                format: ContactExportFormat::VCard,
            })
        }
        ContactExportFormat::Csv | ContactExportFormat::Ldif => {
            let contacts: Vec<VCardContact> = selected.iter().map(|r| r.contact.clone()).collect();
            let text = export_contacts(&contacts, format, None)?;
            let initial_name = if selected.len() == 1 {
                let name = &selected[0].contact.fn_name;
                let safe_stem = if name.trim().is_empty() {
                    "kontakt".to_string()
                } else {
                    name.chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        .collect::<String>()
                };
                let stem = if safe_stem.is_empty() {
                    "kontakt"
                } else {
                    &safe_stem
                };
                format!("{stem}.{}", format.extension())
            } else {
                format!("kontakte.{}", format.extension())
            };
            Ok(ContactExportPlan {
                initial_name,
                payload: text.into_bytes(),
                contact_count: selected.len(),
                format,
            })
        }
    }
}

fn save_contact_export_async(button: &gtk::Button, file: gio::File, plan: ContactExportPlan) {
    let button = button.clone();
    let contact_count = plan.contact_count;
    let format = plan.format;
    file.replace_contents_async(
        plan.payload,
        None,
        true,
        gio::FileCreateFlags::NONE,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(_) if contact_count == 1 => show_toast_for_widget(
                &button,
                &format!("Kontakt als {} exportiert", format.display_label()),
            ),
            Ok(_) => show_toast_for_widget(
                &button,
                &format!(
                    "{contact_count} Kontakte als {} exportiert",
                    format.display_label()
                ),
            ),
            Err((_, error)) => show_toast_for_widget(
                &button,
                &format!("Export konnte nicht gespeichert werden: {error}"),
            ),
        },
    );
}

fn reset_contact_detail(
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
) {
    detail_avatar.set_text(Some(""));
    detail_avatar.set_custom_image(None::<&gdk::Texture>);
    detail_name.set_label("Kein Kontakt ausgewählt");
    detail_title.set_label("Adressbuchdaten werden nach der Synchronisation angezeigt");
    detail_extra.set_label("Keine weiteren Kontaktdaten");
    email_row.set_title("Keine E-Mail-Adresse");
    email_row.set_subtitle("Kein Kontakt ausgewählt");
}

#[allow(clippy::too_many_arguments)]
fn sync_contact_selection(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
    selected_contact: &Rc<RefCell<Option<EBookContactRecord>>>,
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
    email_button: &gtk::Button,
    export_button: &gtk::Button,
    print_button: &gtk::Button,
    edit_button: &gtk::Button,
    merge_button: &gtk::Button,
    delete_button: &gtk::Button,
    copy_button: &gtk::Button,
    transfer_copy_button: &gtk::Button,
    transfer_move_button: &gtk::Button,
) {
    let selected = match selected_contact_records_checked(contact_list, stored_contacts) {
        Ok(selected) => selected,
        Err(error) if error == "Keine Kontakte ausgewählt" => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, "Contact selection no longer maps to the loaded records");
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            reset_contact_detail(
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            apply_contact_selection_action_state(
                ContactSelectionActionState {
                    selection_count: 0,
                    email: false,
                    export: false,
                    edit: false,
                    merge: false,
                    delete: false,
                    copy: false,
                    transfer_copy: false,
                    transfer_move: false,
                    print: false,
                },
                email_button,
                export_button,
                edit_button,
                merge_button,
                delete_button,
                print_button,
                copy_button,
                transfer_copy_button,
                transfer_move_button,
            );
            return;
        }
    };
    let state = contact_selection_action_state(&selected, &stored_contacts.borrow());

    match selected.as_slice() {
        [] => {
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                flow.unselect_all();
            }
            reset_contact_detail(
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
        }
        [record] => {
            *selected_contact.borrow_mut() = Some(record.clone());
            page.imp()
                .selected_identity
                .replace(Some(ContactIdentity::new(
                    record.source_uid.clone(),
                    record.uid.clone(),
                )));
            if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                select_contact_card(flow, &record.source_uid, &record.uid);
            }
            update_contact_detail(
                record,
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
        }
        records => {
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
                flow.unselect_all();
            }
            detail_avatar.set_text(Some(""));
            detail_avatar.set_custom_image(None::<&gdk::Texture>);
            detail_name.set_label(&format!("{} Kontakte ausgewählt", records.len()));
            detail_title.set_label(
                "E-Mail, Drucken, Löschen und vCard-Export verwenden die vollständige Auswahl; Bearbeiten und Zusammenführen benötigen genau einen Kontakt.",
            );
            email_row.set_title(&format!("{} Kontakte ausgewählt", records.len()));
            email_row.set_subtitle(if state.email {
                "E-Mail-Adressen können gemeinsam verwendet werden"
            } else {
                "Keine E-Mail-Adressen in der Auswahl"
            });
            detail_extra.set_label(if state.delete {
                "Mehrfachauswahl: Ausgewählte Kontakte können gemeinsam gedruckt, exportiert oder gelöscht werden."
            } else {
                "Mehrfachauswahl: Drucken und Export schreiben die geprüften vCards. Löschen ist für schreibgeschützte Quellen deaktiviert."
            });
        }
    }
    let selected_ids = current_contact_selected_identities(contact_list);
    if let Some(layout) = page.imp().contact_table_layout.borrow_mut().as_mut() {
        for row in layout.rows.iter_mut() {
            row.selected = selected_ids.contains(&row.identity);
        }
    }
    page.queue_contacts_table_draw();
    apply_contact_selection_action_state(
        state,
        email_button,
        export_button,
        edit_button,
        merge_button,
        delete_button,
        print_button,
        copy_button,
        transfer_copy_button,
        transfer_move_button,
    );
}

fn clear_contact_selection(page: &ContactsPage, contact_list: &gtk::ListBox) {
    contact_list.unselect_all();
    if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
        flow.unselect_all();
    }
    page.imp().selected_identity.replace(None);
    if let Some(layout) = page.imp().contact_table_layout.borrow_mut().as_mut() {
        for row in layout.rows.iter_mut() {
            row.selected = false;
        }
    }
    page.queue_contacts_table_draw();
}

fn should_restore_contact_selection(
    selection_restore: Option<&ContactSelectionRestore>,
    current_identity: &ContactIdentity,
    generation: u64,
) -> bool {
    selection_restore.is_some_and(|target| {
        target.generation == generation && target.identity == *current_identity
    })
}

fn update_alphabet_pane(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
    search_state: &ContactPageSearchState,
) {
    let Some(alphabet_box) = page.imp().alphabet_box.borrow().clone() else {
        return;
    };

    while let Some(child) = alphabet_box.first_child() {
        alphabet_box.remove(&child);
    }

    let visible_set = search_state.visible_identities.borrow();
    let mut letter_first_row: std::collections::BTreeMap<char, (String, gtk::ListBoxRow, String, String)> =
        std::collections::BTreeMap::new();

    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let source_uid = unsafe {
            row.data::<String>("contact-source-uid")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default()
        };
        let uid = unsafe {
            row.data::<String>("contact-id")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default()
        };
        if !visible_set.contains(&ContactIdentity::new(source_uid.clone(), uid.clone())) {
            continue;
        }
        let sort_key = unsafe {
            row.data::<String>("contact-sort-key")
                .map(|v| v.as_ref().clone())
                .unwrap_or_default()
        };
        if let Some(ch) = sort_key.chars().find(|c| c.is_alphabetic()) {
            let ch_upper = ch.to_ascii_uppercase();
            match letter_first_row.entry(ch_upper) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert((sort_key, row, source_uid, uid));
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if sort_key < e.get().0 {
                        e.insert((sort_key, row, source_uid, uid));
                    }
                }
            }
        }
    }

    for (letter, (_, target_row, target_source_uid, target_uid)) in letter_first_row {
        let button = gtk::Button::builder()
            .label(letter.to_string())
            .css_classes(vec!["contacts-alphabet-btn"])
            .tooltip_text(format!("Zu '{letter}' springen"))
            .halign(gtk::Align::End)
            .build();

        let target_row_clone = target_row.clone();
        let contact_list_clone = contact_list.clone();
        let page_weak = page.downgrade();
        let s_uid = target_source_uid.clone();
        let u_id = target_uid.clone();
        button.connect_clicked(move |_| {
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if page.imp().is_syncing_selection.get() {
                return;
            }

            select_contact_row(&contact_list_clone, &s_uid, &u_id);

            let is_cards = page
                .imp()
                .view_stack
                .borrow()
                .as_ref()
                .and_then(|stack| stack.visible_child_name())
                .is_none_or(|name| name == "cards");

            if is_cards {
                if let (Some(flow), Some(cards_scroll)) = (
                    page.imp().contact_cards_flow.borrow().as_ref(),
                    page.imp().cards_scroll.borrow().as_ref(),
                ) {
                    if let Some(card_child) = find_card_child(flow, &s_uid, &u_id) {
                        if card_child.is_mapped() {
                            card_child.grab_focus();
                            #[allow(deprecated)]
                            let coords = card_child.translate_coordinates(flow, 0.0, 0.0);
                            if let Some((_, y)) = coords {
                                let vadj = cards_scroll.vadjustment();
                                vadj.set_value(y.clamp(vadj.lower(), (vadj.upper() - vadj.page_size()).max(vadj.lower())));
                            }
                        }
                    }
                }
            } else {
                if let Some(scroll) = page.imp().list_scroll.borrow().as_ref() {
                    if let Some(layout) = page.imp().contact_table_layout.borrow().as_ref() {
                        let letter_upper: String = letter.to_uppercase().collect();
                        if let Some(r) = layout.rows.iter().find(|r| {
                            r.name.to_uppercase().starts_with(&letter_upper)
                                || r.company.to_uppercase().starts_with(&letter_upper)
                        }) {
                            let vadj = scroll.vadjustment();
                            vadj.set_value(r.y.clamp(vadj.lower(), (vadj.upper() - vadj.page_size()).max(vadj.lower())));
                        }
                    }
                }
                if target_row_clone.is_mapped() {
                    target_row_clone.grab_focus();
                    if let Some(scroll) = page.imp().list_scroll.borrow().as_ref() {
                        #[allow(deprecated)]
                        let coords = target_row_clone.translate_coordinates(&contact_list_clone, 0.0, 0.0);
                        if let Some((_, y)) = coords {
                            let vadj = scroll.vadjustment();
                            vadj.set_value(y.clamp(vadj.lower(), (vadj.upper() - vadj.page_size()).max(vadj.lower())));
                        }
                    }
                }
            }
        });

        let list_row = gtk::ListBoxRow::builder()
            .child(&button)
            .activatable(false)
            .selectable(false)
            .halign(gtk::Align::End)
            .css_classes(vec!["contacts-alphabet-row"])
            .build();
        alphabet_box.append(&list_row);
    }
}

fn apply_contact_search_selection(
    page: &ContactsPage,
    contact_list: &gtk::ListBox,
    selection_restore: Option<&ContactSelectionRestore>,
    identities: &[ContactIdentity],
    generation: u64,
    search_state: &ContactPageSearchState,
) {
    let selection_before_apply = selected_contact_identity(page, contact_list);
    contact_list.invalidate_filter();
    if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
        flow.invalidate_filter();
    }
    update_alphabet_pane(page, contact_list, search_state);
    page.refresh_table_canvas();

    let Some(current_identity) = selection_before_apply else {
        return;
    };
    if !identities.contains(&current_identity) {
        clear_contact_selection(page, contact_list);
        return;
    }

    if should_restore_contact_selection(selection_restore, &current_identity, generation) {
        let restored = select_contact_row(
            contact_list,
            &current_identity.source_uid,
            &current_identity.uid,
        );
        if !restored {
            clear_contact_selection(page, contact_list);
        } else if let Some(flow) = page.imp().contact_cards_flow.borrow().as_ref() {
            select_contact_card(flow, &current_identity.source_uid, &current_identity.uid);
        }
    }
}

fn start_contact_search(
    page_weak: glib::WeakRef<ContactsPage>,
    query: ContactSearchQuery,
    search_state: Rc<ContactPageSearchState>,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let Some(page) = page_weak.upgrade() else {
        return;
    };
    if page.imp().disposed.get() {
        return;
    }

    // Invalidate the previous generation before validating the new input.
    // Unsupported/invalid input therefore cannot let an older worker publish
    // after the user has already moved on, while the last valid result stays
    // visible below the status message.
    let request = search_state.generations.begin();
    let generation = request.generation();
    page.imp()
        .contact_search_cancel
        .replace(Some(request.cancellation()));

    if let Err(error) = query.validate() {
        if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
            status.set_text(&format!("Suche nicht angewendet: {error}"));
            status.set_visible(true);
        }
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.stop();
            spinner.set_visible(false);
        }
        return;
    }
    search_state.set_active_query(query.clone());

    let selection_restore = page
        .imp()
        .contact_list
        .borrow()
        .as_ref()
        .and_then(|contact_list| {
            selected_contact_identity(&page, contact_list).map(|identity| ContactSelectionRestore {
                generation,
                identity,
            })
        });
    if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
        status.set_text("Kontakte werden gesucht…");
        status.set_visible(true);
    }
    if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
        spinner.start();
        spinner.set_visible(true);
    }

    let snapshots = {
        let contacts = stored_contacts.borrow();
        let Some(snapshots) = snapshot_records_cancelable(&contacts, &request.cancellation())
        else {
            return;
        };
        snapshots
    };
    let receiver = search_state.worker.submit(
        search_state.generations.clone(),
        request.clone(),
        query.clone(),
        snapshots,
    );

    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().disposed.get()
            || search_state.generations.current_generation() != generation
            || !request.is_current()
        {
            return;
        }
        let evaluation = match result {
            Ok(evaluation) => evaluation,
            Err(error) => {
                if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
                    status.set_text(&format!("Suche nicht angewendet: {error}"));
                    status.set_visible(true);
                }
                if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                    spinner.stop();
                    spinner.set_visible(false);
                }
                page.imp().contact_search_cancel.borrow_mut().take();
                return;
            }
        };
        let ContactSearchEvaluation::Results { identities, .. } = evaluation else {
            return;
        };
        let identity_set = identities.iter().cloned().collect::<HashSet<_>>();
        search_state.visible_identities.replace(identity_set);
        search_state
            .last_valid
            .borrow_mut()
            .accept_results(query, identities.clone());

        if let Some(contact_list) = page.imp().contact_list.borrow().as_ref() {
            apply_contact_search_selection(
                &page,
                contact_list,
                selection_restore.as_ref(),
                &identities,
                generation,
                &search_state,
            );
        }
        if let Some(status) = page.imp().contact_search_status.borrow().as_ref() {
            if identities.is_empty() {
                status.set_text("Keine Kontakte entsprechen der Suche");
                status.set_visible(true);
            } else {
                status.set_visible(false);
            }
        }
        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
            spinner.stop();
            spinner.set_visible(false);
        }
        page.imp().contact_search_cancel.borrow_mut().take();
    });
}

fn reapply_active_contact_search(
    page_weak: glib::WeakRef<ContactsPage>,
    search_state: Rc<ContactPageSearchState>,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let query = search_state.active_query();
    start_contact_search(page_weak, query, search_state, stored_contacts);
}

fn contact_category_matches(categories: &str, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return true;
    }
    categories
        .split('\u{1f}')
        .any(|category| category.trim().eq_ignore_ascii_case(filter))
}

fn normalize_contact_query(value: &str) -> String {
    crate::services::contact_search_service::normalize_search_text(value)
}

fn contact_query_matches(search_text: &str, query: &str) -> bool {
    let normalized_query = normalize_contact_query(query);
    if normalized_query.is_empty() {
        return true;
    }
    let normalized_search_text = normalize_contact_query(search_text);
    normalized_query
        .split_whitespace()
        .all(|term| normalized_search_text.contains(term))
}

fn refresh_contact_category_model(
    category_model: &gtk::StringList,
    category_selector: &gtk::DropDown,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
    selected_category: &Rc<RefCell<String>>,
) {
    let mut categories = stored_contacts
        .borrow()
        .iter()
        .flat_map(|record| record.contact.categories.iter())
        .map(|category| category.trim().to_string())
        .filter(|category| !category.is_empty())
        .collect::<Vec<_>>();
    categories.sort_by_cached_key(|category| category.to_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let current = selected_category.borrow().clone();
    let selected = current
        .is_empty()
        .then_some(0)
        .or_else(|| {
            categories
                .iter()
                .position(|category| category.eq_ignore_ascii_case(&current))
                .map(|index| index as u32 + 1)
        })
        .unwrap_or(0);
    if !current.is_empty() && selected == 0 {
        selected_category.replace(String::new());
    }

    let mut names = vec!["Alle Kategorien".to_string()];
    names.extend(categories);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    category_model.splice(0, category_model.n_items(), &name_refs);
    if category_selector.selected() != selected {
        category_selector.set_selected(selected);
    }
}

fn upsert_contact_row(
    contact_list: &gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    record: EBookContactRecord,
) {
    let existing_index = stored_contacts.borrow().iter().position(|existing| {
        existing.source_uid == record.source_uid && existing.uid == record.uid
    });
    if let Some(index) = existing_index {
        stored_contacts.borrow_mut().remove(index);
        remove_contact_list_row(contact_list, &record.source_uid, &record.uid);
    }
    append_contact_row(contact_list, &record);
    stored_contacts.borrow_mut().push(record);
    mark_duplicate_rows(contact_list, &stored_contacts);
}

fn remove_contact_row(
    contact_list: &gtk::ListBox,
    stored_contacts: Rc<RefCell<Vec<EBookContactRecord>>>,
    source_uid: &str,
    uid: &str,
) -> bool {
    let Some(index) = stored_contacts
        .borrow()
        .iter()
        .position(|contact| contact.source_uid == source_uid && contact.uid == uid)
    else {
        return false;
    };
    stored_contacts.borrow_mut().remove(index);
    remove_contact_list_row(contact_list, source_uid, uid);
    mark_duplicate_rows(contact_list, &stored_contacts);
    true
}

fn mark_duplicate_rows(
    contact_list: &gtk::ListBox,
    stored_contacts: &Rc<RefCell<Vec<EBookContactRecord>>>,
) {
    let contacts = stored_contacts.borrow();
    let mut key_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(contacts.len() * 3);
    for contact in contacts.iter() {
        for key in contact.contact.duplicate_keys() {
            *key_counts.entry(key).or_insert(0) += 1;
        }
    }

    let mut duplicate_identities: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for contact in contacts.iter() {
        if contact
            .contact
            .duplicate_keys()
            .iter()
            .any(|k| key_counts.get(k).copied().unwrap_or(0) > 1)
        {
            duplicate_identities.insert((contact.source_uid.clone(), contact.uid.clone()));
        }
    }

    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let (source_uid, uid) = unsafe {
            (
                row.data::<String>("contact-source-uid")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default(),
                row.data::<String>("contact-id")
                    .map(|value| value.as_ref().clone())
                    .unwrap_or_default(),
            )
        };
        let duplicate = duplicate_identities.contains(&(source_uid, uid));
        unsafe {
            row.set_data("contact-duplicate", duplicate);
        }
        if duplicate {
            row.add_css_class("warning");
        } else {
            row.remove_css_class("warning");
        }
    }
    contact_list.invalidate_filter();
}

fn contact_has_duplicate(contact: &EBookContactRecord, contacts: &[EBookContactRecord]) -> bool {
    contacts
        .iter()
        .any(|other| contact_records_share_duplicate_key(contact, other))
}

fn contact_has_mergeable_duplicate(
    contact: &EBookContactRecord,
    contacts: &[EBookContactRecord],
) -> bool {
    contacts
        .iter()
        .any(|other| contact_records_are_duplicates(contact, other))
}

fn contact_records_are_duplicates(
    primary: &EBookContactRecord,
    candidate: &EBookContactRecord,
) -> bool {
    primary.source_uid == candidate.source_uid
        && contact_records_share_duplicate_key(primary, candidate)
}

fn contact_records_share_duplicate_key(
    primary: &EBookContactRecord,
    candidate: &EBookContactRecord,
) -> bool {
    if primary.source_uid == candidate.source_uid && primary.uid == candidate.uid {
        return false;
    }
    let keys = primary.contact.duplicate_keys();
    let candidate_keys = candidate.contact.duplicate_keys();
    keys.iter().any(|key| candidate_keys.contains(key))
}

fn remove_contact_list_row(contact_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = contact_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("contact-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = row
                .data::<String>("contact-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            contact_list.remove(&row);
            return;
        }
    }
}

fn update_contact_detail(
    record: &EBookContactRecord,
    detail_avatar: &adw::Avatar,
    detail_name: &gtk::Label,
    detail_title: &gtk::Label,
    detail_extra: &gtk::Label,
    email_row: &adw::ActionRow,
) {
    detail_avatar.set_text(Some(&record.contact.fn_name));
    set_avatar_photo(detail_avatar, record.contact.photo.as_ref());
    detail_name.set_label(&record.contact.fn_name);
    let email_text = contact_email_text(&record.contact);
    let phone_text = contact_phone_text(&record.contact);
    email_row.set_title(if email_text.is_empty() {
        "Keine E-Mail-Adresse"
    } else {
        email_text.as_str()
    });
    email_row.set_subtitle(if phone_text.is_empty() {
        "Keine Telefonnummer"
    } else {
        phone_text.as_str()
    });
    let detail_title_text = contact_detail_title(&record.contact);
    detail_title.set_label(&detail_title_text);
    detail_extra.set_label(&contact_detail_summary(&record.contact));
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Contact UI result");
}

fn append_contact_row(contact_list: &gtk::ListBox, record: &EBookContactRecord) {
    let contact = &record.contact;
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    unsafe {
        let search_text = contact_record_search_text(record);
        row.set_data("contact-name", contact.fn_name.clone());
        row.set_data("contact-sort-key", contact_sort_key(contact));
        row.set_data("contact-email", contact.email.clone());
        row.set_data("contact-title", contact.title.clone());
        row.set_data("contact-search", search_text);
        row.set_data("contact-id", record.uid.clone());
        row.set_data("contact-source-uid", record.source_uid.clone());
        row.set_data("contact-source-name", record.source_display_name.clone());
        row.set_data("contact-vcard", record.vcard.clone());
        row.set_data("contact-categories", contact.categories.join("\u{1f}"));
    }

    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let avatar = adw::Avatar::builder()
        .size(40)
        .text(&contact.fn_name)
        .show_initials(true)
        .build();
    set_avatar_photo(&avatar, contact.photo.as_ref());
    row_box.append(&avatar);

    let info_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let name = gtk::Label::builder()
        .label(&contact.fn_name)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(vec!["heading"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(1)
        .width_chars(1)
        .build();
    let email_text = contact_email_text(contact);
    let mut subtitle_parts = Vec::new();
    if !contact.org.is_empty() {
        subtitle_parts.push(contact.org.as_str());
    } else if !contact.title.is_empty() {
        subtitle_parts.push(contact.title.as_str());
    }
    if !email_text.is_empty() {
        subtitle_parts.push(email_text.as_str());
    }
    let subtitle_text = if subtitle_parts.is_empty() {
        "Keine Kontaktdaten".to_string()
    } else {
        subtitle_parts.join(" • ")
    };
    let subtitle = gtk::Label::builder()
        .label(&subtitle_text)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .css_classes(vec!["dim-label", "caption"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(1)
        .width_chars(1)
        .build();
    info_box.append(&name);
    info_box.append(&subtitle);
    row_box.append(&info_box);

    if !contact.tel.is_empty() {
        let phone_label = gtk::Label::builder()
            .label(&contact.tel)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .css_classes(vec!["dim-label", "caption"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        row_box.append(&phone_label);
    }

    row.set_child(Some(&row_box));
    contact_list.append(&row);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContactCardPhone {
    pub label: String,
    pub number: String,
    pub icon: &'static str,
}

pub(crate) fn contact_card_phones(contact: &VCardContact) -> Vec<ContactCardPhone> {
    if contact.is_list {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut seen_numbers = std::collections::HashSet::new();

    // 1. Check phone_properties for TYPE parameters first
    for prop in &contact.phone_properties {
        let number = prop.value.trim();
        if number.is_empty() || seen_numbers.contains(number) {
            continue;
        }
        seen_numbers.insert(number.to_string());
        let prop_upper = prop.property.to_ascii_uppercase();
        let (label, icon) = if prop_upper.contains("CELL") || prop_upper.contains("MOBILE") {
            ("Mobil", "call-start-symbolic")
        } else if prop_upper.contains("WORK") || prop_upper.contains("BUSINESS") {
            ("Geschäftlich", "call-start-symbolic")
        } else if prop_upper.contains("HOME") {
            ("Privat", "call-start-symbolic")
        } else if prop_upper.contains("FAX") {
            ("Fax", "printer-symbolic")
        } else {
            ("Telefon", "call-start-symbolic")
        };
        result.push(ContactCardPhone {
            label: label.to_string(),
            number: number.to_string(),
            icon,
        });
    }

    // 2. Fallback to contact.phones for any numbers not covered in phone_properties
    let unassigned: Vec<&String> = contact
        .phones
        .iter()
        .filter(|p| {
            let t = p.trim();
            !t.is_empty() && !seen_numbers.contains(t)
        })
        .collect();

    if !unassigned.is_empty() {
        let total_count = result.len() + unassigned.len();
        for (idx, p) in unassigned.into_iter().enumerate() {
            let t = p.trim().to_string();
            seen_numbers.insert(t.clone());
            let (lbl, icn) = if total_count == 1 {
                ("", "call-start-symbolic")
            } else {
                match idx + result.len() {
                    0 => ("Mobil", "call-start-symbolic"),
                    1 => ("Geschäftlich", "call-start-symbolic"),
                    2 => ("Privat", "call-start-symbolic"),
                    _ => ("Telefon", "call-start-symbolic"),
                }
            };
            result.push(ContactCardPhone {
                label: lbl.to_string(),
                number: t,
                icon: icn,
            });
        }
    }

    // 3. Fallback to contact.tel if still empty
    if result.is_empty() && !contact.tel.trim().is_empty() {
        let t = contact.tel.trim().to_string();
        result.push(ContactCardPhone {
            label: String::new(),
            number: t,
            icon: "call-start-symbolic",
        });
    }

    // If there is only one phone number overall and its label is generic "Telefon", clear it
    if result.len() == 1 && result[0].label == "Telefon" {
        result[0].label.clear();
    }

    result
}

fn append_contact_card(flow: &gtk::FlowBox, record: &EBookContactRecord) {
    let contact = &record.contact;
    let card_child = gtk::FlowBoxChild::new();
    card_child.set_focusable(true);
    card_child.add_css_class("contact-card-child");
    card_child.set_size_request(200, 124);
    card_child.set_halign(gtk::Align::Fill);
    card_child.set_valign(gtk::Align::Fill);
    card_child.set_hexpand(true);
    card_child.set_hexpand_set(true);
    card_child.set_vexpand(false);
    card_child.set_vexpand_set(true);

    unsafe {
        let search_text = contact_record_search_text(record);
        card_child.set_data("contact-name", contact.fn_name.clone());
        card_child.set_data("contact-sort-key", contact_sort_key(contact));
        card_child.set_data("contact-email", contact.email.clone());
        card_child.set_data("contact-title", contact.title.clone());
        card_child.set_data("contact-search", search_text);
        card_child.set_data("contact-id", record.uid.clone());
        card_child.set_data("contact-source-uid", record.source_uid.clone());
        card_child.set_data("contact-source-name", record.source_display_name.clone());
        card_child.set_data("contact-vcard", record.vcard.clone());
        card_child.set_data("contact-categories", contact.categories.join("\u{1f}"));
    }

    let card_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(vec!["card", "contact-card-box"])
        .width_request(200)
        .height_request(124)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .hexpand(true)
        .vexpand(false)
        .build();

    let header_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();

    let avatar = adw::Avatar::builder()
        .size(38)
        .text(&contact.fn_name)
        .show_initials(true)
        .build();
    set_avatar_photo(&avatar, contact.photo.as_ref());
    header_box.append(&avatar);

    let title_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();

    let name_label = gtk::Label::builder()
        .label(&contact.fn_name)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .width_chars(1)
        .max_width_chars(15)
        .css_classes(vec!["contact-card-name"])
        .lines(1)
        .build();
    title_box.append(&name_label);

    let sub = if !contact.title.is_empty() && !contact.org.is_empty() {
        format!("{} • {}", contact.title, contact.org)
    } else if !contact.title.is_empty() {
        contact.title.clone()
    } else if !contact.org.is_empty() {
        contact.org.clone()
    } else {
        String::new()
    };
    if !sub.is_empty() {
        let sub_label = gtk::Label::builder()
            .label(&sub)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(15)
            .css_classes(vec!["dim-label", "contact-card-sub"])
            .lines(1)
            .build();
        title_box.append(&sub_label);
    }
    header_box.append(&title_box);

    if let Some(cat) = contact.categories.first().filter(|c| !c.trim().is_empty()) {
        let badge = gtk::Label::builder()
            .label(cat)
            .valign(gtk::Align::Start)
            .halign(gtk::Align::End)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(8)
            .width_chars(1)
            .css_classes(vec!["caption", "contact-card-badge"])
            .build();
        header_box.append(&badge);
    }

    card_box.append(&header_box);

    let details_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .valign(gtk::Align::Start)
        .vexpand(false)
        .hexpand(true)
        .build();

    let mut detail_rows = 0;
    const MAX_DETAIL_ROWS: usize = 2;

    let email_text = contact_email_text(contact);
    if !email_text.is_empty() && detail_rows < MAX_DETAIL_ROWS {
        let email_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        let icon = gtk::Image::from_icon_name("mail-unread-symbolic");
        icon.add_css_class("dim-label");
        icon.set_pixel_size(14);
        email_row.append(&icon);
        let label = gtk::Label::builder()
            .label(&email_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(17)
            .css_classes(vec!["contact-card-detail-text"])
            .lines(1)
            .build();
        email_row.append(&label);
        details_box.append(&email_row);
        detail_rows += 1;
    }

    let card_phones = contact_card_phones(contact);
    for phone in card_phones {
        if detail_rows >= MAX_DETAIL_ROWS {
            break;
        }
        let phone_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        let icon = gtk::Image::from_icon_name(phone.icon);
        icon.add_css_class("dim-label");
        icon.set_pixel_size(14);
        phone_row.append(&icon);
        let display_text = if phone.label.is_empty() {
            phone.number
        } else {
            format!("{}: {}", phone.label, phone.number)
        };
        let label = gtk::Label::builder()
            .label(&display_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(17)
            .css_classes(vec!["contact-card-detail-text"])
            .lines(1)
            .build();
        phone_row.append(&label);
        details_box.append(&phone_row);
        detail_rows += 1;
    }

    if detail_rows < MAX_DETAIL_ROWS {
        if let Some(addr) = contact.addresses.first().filter(|a| !a.trim().is_empty()) {
            let parts: Vec<&str> = addr.split(';').map(str::trim).filter(|p| !p.is_empty()).collect();
            let display_addr = if parts.is_empty() {
                addr.trim().replace('\n', ", ")
            } else {
                parts.join(", ")
            };
            if !display_addr.is_empty() {
                let addr_row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .valign(gtk::Align::Center)
                    .hexpand(true)
                    .build();
                let icon = gtk::Image::from_icon_name("mark-location-symbolic");
                icon.add_css_class("dim-label");
                icon.set_pixel_size(14);
                addr_row.append(&icon);
                let label = gtk::Label::builder()
                    .label(&display_addr)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .width_chars(1)
                    .max_width_chars(17)
                    .css_classes(vec!["contact-card-detail-text"])
                    .lines(1)
                    .build();
                addr_row.append(&label);
                details_box.append(&addr_row);
                detail_rows += 1;
            }
        }
    }

    card_box.append(&details_box);
    card_child.set_child(Some(&card_box));
    flow.append(&card_child);
}

fn find_card_child(
    flow: &gtk::FlowBox,
    source_uid: &str,
    uid: &str,
) -> Option<gtk::FlowBoxChild> {
    let mut child = flow.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(card_child) = widget.downcast::<gtk::FlowBoxChild>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = card_child
                .data::<String>("contact-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = card_child
                .data::<String>("contact-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            return Some(card_child);
        }
    }
    None
}

fn select_contact_card(flow: &gtk::FlowBox, target_source_uid: &str, target_uid: &str) -> bool {
    if let Some(card_child) = find_card_child(flow, target_source_uid, target_uid) {
        if !card_child.is_selected() {
            flow.unselect_all();
            flow.select_child(&card_child);
        }
        true
    } else {
        flow.unselect_all();
        false
    }
}

fn remove_contact_card(flow: &gtk::FlowBox, source_uid: &str, uid: &str) -> bool {
    if let Some(card_child) = find_card_child(flow, source_uid, uid) {
        flow.remove(&card_child);
        true
    } else {
        false
    }
}

fn upsert_contact_card(flow: &gtk::FlowBox, record: &EBookContactRecord) {
    remove_contact_card(flow, &record.source_uid, &record.uid);
    append_contact_card(flow, record);
}

fn contact_record_search_text(record: &EBookContactRecord) -> String {
    normalize_contact_query(&field_values(&record.contact, ContactSearchField::AnyField).join(" "))
}

fn contact_sort_key(contact: &VCardContact) -> String {
    let family = contact.name.family.trim();
    let given = contact.name.given.trim();
    let name = if family.is_empty() {
        contact.fn_name.trim().to_string()
    } else if given.is_empty() {
        family.to_string()
    } else {
        format!("{family} {given}")
    };
    name.to_lowercase()
}

fn set_avatar_photo(
    avatar: &adw::Avatar,
    photo: Option<&crate::services::vcard_service::VCardPhoto>,
) {
    let Some(bytes) = photo.and_then(|photo| photo.decoded_bytes()) else {
        avatar.set_custom_image(None::<&gdk::Texture>);
        return;
    };
    let bytes = glib::Bytes::from_owned(bytes);
    match gdk::Texture::from_bytes(&bytes) {
        Ok(texture) => avatar.set_custom_image(Some(&texture)),
        Err(error) => {
            avatar.set_custom_image(None::<&gdk::Texture>);
            tracing::debug!(%error, "Could not decode vCard avatar photo");
        }
    }
}

fn contact_email_text(contact: &VCardContact) -> String {
    if contact.is_list {
        if contact.list_show_addresses {
            return contact.list_member_emails().join(", ");
        }
        return format!(
            "Kontaktliste · {} Mitglied(e) · Adressen verborgen",
            contact.list_members.len()
        );
    }
    if contact.emails.is_empty() {
        contact.email.clone()
    } else {
        contact.emails.join(", ")
    }
}

fn contact_recipient_text(contact: &VCardContact) -> String {
    contact_recipient_text_with_records(contact, &[])
}

fn contact_recipient_text_with_records(
    contact: &VCardContact,
    records: &[EBookContactRecord],
) -> String {
    if !contact.is_list {
        return contact_email_text(contact);
    }
    let mut recipients = Vec::new();
    append_list_recipients(contact, records, &mut Vec::new(), &mut recipients);
    recipients.join(", ")
}

fn append_list_recipients(
    contact: &VCardContact,
    records: &[EBookContactRecord],
    visited: &mut Vec<String>,
    recipients: &mut Vec<String>,
) {
    for member in &contact.list_members {
        if !member.email.trim().is_empty() {
            push_unique_recipient(recipients, &member.email);
        }
        let Some(member_uid) = member
            .contact_uid
            .trim()
            .strip_prefix("urn:uuid:")
            .or_else(|| {
                (!member.contact_uid.trim().is_empty()).then_some(member.contact_uid.trim())
            })
        else {
            continue;
        };
        if !matches!(
            member.kind,
            VCardListMemberKind::NestedList | VCardListMemberKind::StandardMember
        ) {
            continue;
        }
        if visited.iter().any(|uid| uid == member_uid) {
            continue;
        }
        let Some(record) = records.iter().find(|record| record.uid == member_uid) else {
            continue;
        };
        visited.push(member_uid.to_string());
        if record.contact.is_list {
            append_list_recipients(&record.contact, records, visited, recipients);
        } else {
            for email in if record.contact.emails.is_empty() {
                std::slice::from_ref(&record.contact.email)
            } else {
                record.contact.emails.as_slice()
            } {
                if !email.trim().is_empty() {
                    push_unique_recipient(recipients, email);
                }
            }
        }
        visited.pop();
    }
}

fn push_unique_recipient(recipients: &mut Vec<String>, email: &str) {
    let email = email.trim();
    if !email.is_empty()
        && !recipients
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(email))
    {
        recipients.push(email.to_string());
    }
}

fn contact_phone_text(contact: &VCardContact) -> String {
    if contact.is_list {
        return String::new();
    }
    if contact.phones.is_empty() {
        contact.tel.clone()
    } else {
        contact.phones.join(", ")
    }
}

fn contact_detail_title(contact: &VCardContact) -> String {
    if contact.is_list {
        format!("Kontaktliste · {} Mitglied(e)", contact.list_members.len())
    } else if contact.title.is_empty() {
        contact.org.clone()
    } else {
        contact.title.clone()
    }
}

pub(crate) fn ordered_contact_addresses(
    contact: &VCardContact,
    home_before_work: bool,
) -> Vec<String> {
    if !home_before_work || contact.address_properties.is_empty() {
        return contact.addresses.clone();
    }
    let mut entries: Vec<(usize, &str, &crate::services::vcard_service::VCardProperty)> = contact
        .addresses
        .iter()
        .zip(contact.address_properties.iter())
        .enumerate()
        .map(|(idx, (addr, prop))| (idx, addr.as_str(), prop))
        .collect();
    entries.sort_by_key(|(idx, _, prop)| {
        let prop_upper = prop.property.to_ascii_uppercase();
        let rank = if prop_upper.contains("HOME") {
            0
        } else if prop_upper.contains("WORK") {
            1
        } else {
            2
        };
        (rank, *idx)
    });
    entries
        .into_iter()
        .map(|(_, addr, _)| addr.to_string())
        .collect()
}

pub(crate) fn contact_detail_summary_with_options(
    contact: &VCardContact,
    home_before_work: bool,
    show_maps: bool,
    map_target: &str,
    address_formatting: bool,
) -> String {
    let mut lines = Vec::new();
    if contact.is_list {
        lines.push(format!(
            "Kontaktliste: {} Mitglied(e)",
            contact.list_members.len()
        ));
        if contact.list_show_addresses {
            let members = contact
                .list_members
                .iter()
                .map(|member| member.display_text(true))
                .collect::<Vec<_>>();
            if !members.is_empty() {
                lines.push(format!("Mitglieder: {}", members.join(" · ")));
            }
        } else {
            lines.push("Mitgliederadressen: verborgen".to_string());
        }
    }
    let ordered_addrs = ordered_contact_addresses(contact, home_before_work);
    if !ordered_addrs.is_empty() {
        let separator = if address_formatting {
            "\n         "
        } else {
            " · "
        };
        lines.push(format!("Adresse: {}", ordered_addrs.join(separator)));
        if show_maps {
            let service_name = if map_target.eq_ignore_ascii_case("google") {
                "Google Maps"
            } else {
                "OpenStreetMap"
            };
            lines.push(format!("Karte: aktiv ({service_name})"));
        }
    }
    if !contact.categories.is_empty() {
        lines.push(format!("Kategorien: {}", contact.categories.join(", ")));
    }
    if !contact.urls.is_empty() {
        lines.push(format!("Websites: {}", contact.urls.join(", ")));
    }
    if !contact.nickname.trim().is_empty() {
        lines.push(format!("Spitzname: {}", contact.nickname));
    }
    if !contact.birthday.trim().is_empty() {
        lines.push(format!("Geburtstag: {}", contact.birthday));
    }
    if !contact.anniversary.trim().is_empty() {
        lines.push(format!("Jahrestag: {}", contact.anniversary));
    }
    if !contact.role.trim().is_empty() {
        lines.push(format!("Rolle: {}", contact.role));
    }
    if !contact.gender.trim().is_empty() {
        lines.push(format!("Geschlecht: {}", contact.gender));
    }
    if !contact.timezone.trim().is_empty() {
        lines.push(format!("Zeitzone: {}", contact.timezone));
    }
    if !contact.geo.trim().is_empty() {
        lines.push(format!("Position: {}", contact.geo));
    }
    if !contact.impps.is_empty() {
        lines.push(format!("Chat/IMPP: {}", contact.impps.join(", ")));
    }
    if !contact.related.is_empty() {
        lines.push(format!("Verwandt: {}", contact.related.join(", ")));
    }
    if !contact.note.trim().is_empty() {
        lines.push(format!("Notiz: {}", contact.note));
    }
    if lines.is_empty() {
        "Keine weiteren Kontaktdaten".to_string()
    } else {
        lines.join("\n")
    }
}

fn contact_detail_summary(contact: &VCardContact) -> String {
    let (home_before_work, show_maps, map_target, address_formatting) = AppSettings::try_new()
        .map(|s| {
            (
                s.contact_preview_home_before_work(),
                s.contact_preview_show_maps(),
                s.contact_open_map_target(),
                s.contact_address_formatting(),
            )
        })
        .unwrap_or_else(|| (false, false, "openstreetmap".to_string(), false));
    contact_detail_summary_with_options(
        contact,
        home_before_work,
        show_maps,
        &map_target,
        address_formatting,
    )
}

impl Default for ContactsPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContactsPage,
        ContactActionState, ContactIdentity, ContactPageSearchState, ContactSelectionRestore,
        ContactTransferKind, ContactVCardExportError, confirmed_contact_uid, contact_action_state,
        contact_category_matches, contact_detail_summary, contact_detail_summary_with_options,
        contact_email_text, contact_load_status_message, contact_query_matches,
        contact_recipient_text, contact_recipient_text_with_records,
        contact_records_are_duplicates, contact_records_share_duplicate_key,
        contact_selection_action_state, contact_sort_key, contact_source_label,
        contact_status_needs_visibility, context_action_state, ordered_contact_addresses,
        plan_contact_delete_batch, plan_contact_export, plan_contact_transfer,
        plan_contact_vcard_export, prepare_vcard_for_transfer, quick_field_for_index, quick_query,
        reduce_contact_delete_result, reduce_contact_transfer_result,
        should_restore_contact_selection, with_contact_outer_filters,
        append_contact_card, find_card_child, select_contact_card, remove_contact_card, upsert_contact_card,
        contact_card_phones, ContactCardPhone,
    };
    use crate::services::contact_export_service::ContactExportFormat;
    use crate::services::contact_search_service::{
        ContactClauseGroup, ContactOuterFilters, ContactSearchClause, ContactSearchField,
        ContactSearchOperator, ContactSearchQuery,
    };
    use crate::services::ebook_service::{EBookContactRecord, EBookDataOrigin};
    use crate::services::vcard_service::VCardContact;
    use gtk::prelude::*;
    use gtk::prelude::FlowBoxChildExt;
    use std::cell::RefCell;
    use std::rc::Rc;
    use super::{
        hit_test_contact_table, hit_test_contact_table_header, prepare_contact_table_layout,
        ContactTableColumn,
    };

    fn record(source_uid: &str, uid: &str, contact: VCardContact) -> EBookContactRecord {
        EBookContactRecord {
            source_uid: source_uid.to_string(),
            source_display_name: "Testadressbuch".to_string(),
            uid: uid.to_string(),
            vcard: contact.to_vcard_string(),
            contact,
        }
    }

    #[test]
    fn contact_category_filter_is_case_insensitive() {
        assert!(contact_category_matches("Arbeit\u{1f}Familie", "arbeit"));
        assert!(contact_category_matches("Arbeit\u{1f}Familie", "FAMILIE"));
        assert!(!contact_category_matches("Arbeit\u{1f}Familie", "Privat"));
    }

    #[test]
    fn contact_sort_key_prefers_family_and_given_name() {
        let mut contact = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        contact.name.family = "Lovelace".to_string();
        contact.name.given = "Ada".to_string();

        assert_eq!(contact_sort_key(&contact), "lovelace ada");
    }

    #[test]
    fn contact_sort_key_falls_back_to_display_name() {
        let contact = VCardContact::new("Grace Hopper", "grace@example.test", "", "");

        assert_eq!(contact_sort_key(&contact), "grace hopper");
    }

    #[test]
    fn contact_search_matches_all_terms_without_losing_unicode() {
        let search_text = "Ada Lovelace ada@example.test X-EVOLUTION-TEAM";

        assert!(contact_query_matches(search_text, "ADA example.test"));
        assert!(contact_query_matches(
            search_text,
            "  lovelace   ada@example.test "
        ));
        assert!(!contact_query_matches(search_text, "Ada nicht-vorhanden"));
        assert!(contact_query_matches(search_text, ""));
    }

    #[test]
    fn outer_filters_change_only_the_active_query_filters() {
        let state = ContactPageSearchState::default();
        let advanced = ContactSearchQuery::new(
            ContactClauseGroup::Any,
            vec![
                ContactSearchClause::new(
                    ContactSearchField::Name,
                    ContactSearchOperator::Contains,
                    "Ada",
                ),
                ContactSearchClause::new(
                    ContactSearchField::AnyField,
                    ContactSearchOperator::Contains,
                    "example.test",
                ),
            ],
        );
        state.set_active_query(advanced.clone());
        let updated = state.active_query_with_filters(
            ContactOuterFilters::default()
                .source("book-a")
                .category("Work")
                .unmatched_category(true)
                .duplicate_only(true),
        );

        assert_eq!(updated.group, advanced.group);
        assert_eq!(updated.clauses, advanced.clauses);
        assert_eq!(
            updated.filters,
            ContactOuterFilters {
                source_uid: Some("book-a".to_string()),
                category: Some("Work".to_string()),
                unmatched_category: true,
                duplicate_only: true,
            }
        );
        assert_eq!(state.active_query(), updated);
    }

    #[test]
    fn quick_search_and_field_mapping_parity() {
        assert_eq!(quick_field_for_index(0), ContactSearchField::AnyField);
        assert_eq!(quick_field_for_index(1), ContactSearchField::Name);
        assert_eq!(quick_field_for_index(2), ContactSearchField::Email);
        assert_eq!(quick_field_for_index(3), ContactSearchField::Phone);
        assert_eq!(quick_field_for_index(4), ContactSearchField::Category);
        assert_eq!(quick_field_for_index(99), ContactSearchField::AnyField);

        assert_eq!(
            quick_query(ContactSearchField::AnyField, ""),
            ContactSearchQuery::all()
        );
        assert_eq!(
            quick_query(ContactSearchField::Name, "   "),
            ContactSearchQuery::all()
        );

        let any_query = quick_query(ContactSearchField::AnyField, "Ada Lovelace");
        assert_eq!(any_query.group, ContactClauseGroup::All);
        assert_eq!(
            any_query.clauses,
            vec![ContactSearchClause::new(
                ContactSearchField::AnyField,
                ContactSearchOperator::Contains,
                "Ada Lovelace",
            )]
        );

        let name_query = quick_query(ContactSearchField::Name, "Ada");
        assert_eq!(
            name_query.clauses,
            vec![ContactSearchClause::new(
                ContactSearchField::Name,
                ContactSearchOperator::Contains,
                "Ada",
            )]
        );

        let email_query = quick_query(ContactSearchField::Email, "ada@example.test");
        assert_eq!(
            email_query.clauses,
            vec![ContactSearchClause::new(
                ContactSearchField::Email,
                ContactSearchOperator::Contains,
                "ada@example.test",
            )]
        );
    }

    #[test]
    fn quick_query_composes_with_contact_outer_filters() {
        let book = Rc::new(RefCell::new("book-work".to_string()));
        let category = Rc::new(RefCell::new("VIP".to_string()));
        let base_query = quick_query(ContactSearchField::Name, "Ada");
        let filtered = with_contact_outer_filters(base_query, &book, &category, true, true);
        assert_eq!(filtered.filters.source_uid.as_deref(), Some("book-work"));
        assert_eq!(filtered.filters.category.as_deref(), Some("VIP"));
        assert!(filtered.filters.unmatched_category);
        assert!(filtered.filters.duplicate_only);
        assert_eq!(filtered.clauses.len(), 1);
    }

    #[test]
    fn selection_restoration_is_scoped_to_the_search_generation() {
        let identity = ContactIdentity::new("book-a", "ada");
        let target = ContactSelectionRestore {
            generation: 7,
            identity: identity.clone(),
        };

        assert!(should_restore_contact_selection(
            Some(&target),
            &identity,
            7
        ));
        assert!(!should_restore_contact_selection(
            Some(&target),
            &identity,
            8
        ));
        assert!(!should_restore_contact_selection(None, &identity, 7));
    }

    #[test]
    fn zero_selection_disables_every_selection_action_and_imports_are_read_only() {
        assert_eq!(
            contact_action_state(None, &[]),
            ContactActionState {
                email: false,
                export: false,
                edit: false,
                merge: false,
                delete: false,
                copy: false,
                transfer_copy: false,
                transfer_move: false,
                print: false,
            }
        );

        let imported = record(
            "",
            "imported",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let state = contact_action_state(Some(&imported), std::slice::from_ref(&imported));
        let context_state = context_action_state(Some(&imported), std::slice::from_ref(&imported));
        assert!(state.email);
        assert!(state.export);
        assert!(state.print);
        assert!(!state.edit);
        assert!(!state.merge);
        assert!(!state.delete);
        assert!(state.copy);
        assert!(state.transfer_copy);
        assert!(!state.transfer_move);
        assert_eq!(context_state, state);
    }

    #[test]
    fn multi_selection_keeps_only_batch_safe_actions_enabled() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let grace = record(
            "book-a",
            "grace",
            VCardContact::new("Grace", "grace@example.test", "", ""),
        );
        let state = contact_selection_action_state(
            &[ada.clone(), grace.clone()],
            &[ada.clone(), grace.clone()],
        );

        assert_eq!(state.selection_count, 2);
        assert!(state.email);
        assert!(state.export);
        assert!(state.print);
        assert!(!state.edit);
        assert!(!state.merge);
        assert!(state.delete);
        assert!(!state.copy);
        assert!(state.transfer_copy);
        assert!(state.transfer_move);
    }

    #[test]
    fn contact_selection_action_state_enables_print_for_any_selection() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let grace = record(
            "book-a",
            "grace",
            VCardContact::new("Grace", "grace@example.test", "", ""),
        );

        // 0 selected: print disabled
        let state_empty = contact_selection_action_state(&[], &[]);
        assert!(!state_empty.print);

        // 1 selected: print enabled
        let state_single = contact_selection_action_state(&[ada.clone()], &[ada.clone()]);
        assert!(state_single.print);

        // multiple selected: print enabled
        let state_multi =
            contact_selection_action_state(&[ada.clone(), grace.clone()], &[ada, grace]);
        assert!(state_multi.print);
    }

    #[test]
    fn multi_vcard_export_is_planned_as_one_complete_payload() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let grace = record(
            "book-b",
            "grace",
            VCardContact::new("Grace", "grace@example.test", "", ""),
        );
        let plan = plan_contact_vcard_export(&[ada.clone(), grace.clone()])
            .expect("both complete vCards should form one export");

        assert_eq!(plan.initial_name, "kontakte.vcf");
        assert_eq!(plan.contact_count, 2);
        let payload = String::from_utf8(plan.payload).expect("vCard output is UTF-8");
        assert_eq!(payload.matches("BEGIN:VCARD").count(), 2);
        assert!(payload.contains(&ada.vcard.trim().to_string()));
        assert!(payload.contains(&grace.vcard.trim().to_string()));

        let mut broken = grace;
        broken.vcard = "not a vCard".to_string();
        assert_eq!(
            plan_contact_vcard_export(&[ada, broken]),
            Err(ContactVCardExportError::InvalidVCard {
                contact_name: "Grace".to_string(),
            })
        );
    }

    #[test]
    fn plan_contact_export_supports_csv_and_ldif() {
        let ada = record(
            "book-a",
            "ada",
            VCardContact::new(
                "Ada Lovelace",
                "ada@example.test",
                "Analytical Engine",
                "Mathematician",
            ),
        );
        let grace = record(
            "book-b",
            "grace",
            VCardContact::new("Grace Hopper", "grace@example.test", "US Navy", "Admiral"),
        );

        let csv_plan = plan_contact_export(&[ada.clone(), grace.clone()], ContactExportFormat::Csv)
            .expect("CSV export plan should succeed");
        assert_eq!(csv_plan.format, ContactExportFormat::Csv);
        assert_eq!(csv_plan.contact_count, 2);
        assert_eq!(csv_plan.initial_name, "kontakte.csv");
        let csv_str = String::from_utf8(csv_plan.payload).expect("CSV is utf-8");
        assert!(csv_str.contains("Ada Lovelace"));
        assert!(csv_str.contains("Grace Hopper"));
        assert!(csv_str.contains("Analytical Engine"));

        let ldif_plan = plan_contact_export(&[ada], ContactExportFormat::Ldif)
            .expect("LDIF export plan should succeed");
        assert_eq!(ldif_plan.format, ContactExportFormat::Ldif);
        assert_eq!(ldif_plan.contact_count, 1);
        assert_eq!(ldif_plan.initial_name, "AdaLovelace.ldif");
        let ldif_str = String::from_utf8(ldif_plan.payload).expect("LDIF is utf-8");
        assert!(ldif_str.contains("cn: Ada Lovelace"));
        assert!(ldif_str.contains("mail: ada@example.test"));
    }

    #[test]
    fn source_labels_are_human_readable_and_do_not_expose_uid() {
        assert_eq!(
            contact_source_label("Privates Adressbuch", "goa-secret-source"),
            "Privates Adressbuch"
        );
        assert_eq!(
            contact_source_label("", "local-addressbook"),
            "Lokales Adressbuch"
        );
        assert_eq!(contact_source_label("", "uid-only-source"), "Adressbuch");
    }

    #[test]
    fn load_status_keeps_fallback_and_warning_honest() {
        assert_eq!(
            contact_load_status_message(EBookDataOrigin::Eds, 4, None),
            "4 Kontakt(e) aus synchronisiertem Adressbuch geladen."
        );
        let fallback = contact_load_status_message(
            EBookDataOrigin::Local,
            2,
            Some("Der Adressbuchdienst ist nicht verfügbar"),
        );
        assert!(fallback.contains("lokalem Adressbuch"));
        assert!(fallback.contains("Hinweis:"));
        assert!(contact_status_needs_visibility(0, None));
        assert!(!contact_status_needs_visibility(2, None));
        assert!(contact_status_needs_visibility(2, Some("Warnung")));
    }

    #[test]
    fn duplicate_detection_separates_global_marking_from_safe_merge() {
        let primary = record(
            "book-a",
            "primary",
            VCardContact::new("Ada Lovelace", "ada@example.test", "", ""),
        );
        let same_book = record(
            "book-a",
            "duplicate",
            VCardContact::new("Ada L.", "ADA@example.test", "", ""),
        );
        let other_book = record(
            "book-b",
            "duplicate-other-book",
            VCardContact::new("Ada L.", "ada@example.test", "", ""),
        );

        assert!(contact_records_share_duplicate_key(&primary, &same_book));
        assert!(contact_records_are_duplicates(&primary, &same_book));
        assert!(contact_records_share_duplicate_key(&primary, &other_book));
        assert!(!contact_records_are_duplicates(&primary, &other_book));
        assert!(!contact_records_share_duplicate_key(&primary, &primary));
    }

    #[test]
    fn backend_uid_fallback_preserves_requested_contact_identity() {
        assert_eq!(confirmed_contact_uid("", "contact-42"), "contact-42");
        assert_eq!(
            confirmed_contact_uid("confirmed-42", "contact-42"),
            "confirmed-42"
        );
    }

    #[test]
    fn contact_list_display_hides_addresses_but_keeps_recipients_for_mail() {
        let mut list = VCardContact::new_list("Projektteam", false, Some("list-1"));
        list.list_members = vec![
            crate::services::vcard_service::VCardListMember::external_address(
                "external@example.test",
                "Extern",
            ),
            crate::services::vcard_service::VCardListMember::local_contact(
                "contact-1",
                "local-addressbook",
                "Ada Lovelace",
                "ada@example.test",
                Some(0),
            ),
        ];
        assert!(contact_email_text(&list).contains("Adressen verborgen"));
        assert!(!contact_email_text(&list).contains("external@example.test"));
        assert_eq!(
            contact_recipient_text(&list),
            "external@example.test, ada@example.test"
        );
        assert!(contact_detail_summary(&list).contains("Mitgliederadressen: verborgen"));
    }

    #[test]
    fn nested_list_recipients_are_resolved_without_leaking_hidden_display_text() {
        let mut nested = VCardContact::new_list("Unterteam", true, Some("nested-list"));
        nested.list_members = vec![
            crate::services::vcard_service::VCardListMember::external_address(
                "nested@example.test",
                "Unterteam-Adresse",
            ),
        ];
        let mut outer = VCardContact::new_list("Team", false, Some("outer-list"));
        outer.list_members = vec![
            crate::services::vcard_service::VCardListMember::nested_list(
                "nested-list",
                "Unterteam",
                None,
            ),
        ];
        let nested_record = record("book-a", "nested-list", nested);
        assert_eq!(contact_recipient_text(&outer), "");
        assert_eq!(
            contact_recipient_text_with_records(&outer, &[nested_record]),
            "nested@example.test"
        );
        assert!(contact_email_text(&outer).contains("Adressen verborgen"));
        assert!(!contact_email_text(&outer).contains("nested@example.test"));
    }

    #[test]
    fn plan_contact_delete_batch_rejects_empty_selection() {
        let result = plan_contact_delete_batch(&[]);
        assert_eq!(result, Err("Keine Kontakte ausgewählt".to_string()));
    }

    #[test]
    fn plan_contact_delete_batch_single_contact_creates_named_confirmation() {
        let contact = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        let contact_rec = record("book-a", "ada-uid", contact);
        let plan = plan_contact_delete_batch(&[contact_rec]).expect("plan succeeds");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(
            plan.items[0].identity,
            ContactIdentity::new("book-a", "ada-uid")
        );
        assert_eq!(plan.items[0].name, "Ada Lovelace");
        assert_eq!(plan.confirmation.title, "Kontakt löschen?");
        assert!(plan.confirmation.description.contains("Ada Lovelace"));
        assert!(plan.confirmation.description.contains("Testadressbuch"));
    }

    #[test]
    fn plan_contact_delete_batch_multiple_contacts_creates_count_confirmation() {
        let c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace Hopper", "grace@example.test", "", "");
        let r1 = record("book-a", "ada-uid", c1);
        let r2 = record("book-a", "grace-uid", c2);
        let plan = plan_contact_delete_batch(&[r1, r2]).expect("plan succeeds");
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.confirmation.title, "Kontakte löschen?");
        assert!(
            plan.confirmation
                .description
                .contains("2 ausgewählten Kontakte")
        );
    }

    #[test]
    fn plan_contact_delete_batch_rejects_readonly_imported_source() {
        let contact = VCardContact::new("Imported Readonly", "read@example.test", "", "");
        // Empty source_uid denotes imported/unwritable record
        let contact_rec = record("", "read-uid", contact);
        let result = plan_contact_delete_batch(&[contact_rec]);
        assert_eq!(
            result,
            Err(
                "Mindestens ein ausgewählter Kontakt stammt aus einer schreibgeschützten Quelle"
                    .to_string()
            )
        );
    }

    #[test]
    fn plan_contact_delete_batch_rejects_duplicate_identity() {
        let c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        let r1 = record("book-a", "ada-uid", c1.clone());
        let r2 = record("book-a", "ada-uid", c1);
        let result = plan_contact_delete_batch(&[r1, r2]);
        assert_eq!(
            result,
            Err("Ein ausgewählter Kontakt wurde mehrfach aufgelöst".to_string())
        );
    }

    #[test]
    fn reduce_contact_delete_result_removes_only_confirmed_prefix() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace", "grace@example.test", "", "");
        let c3 = VCardContact::new("Margaret", "margaret@example.test", "", "");
        let r1 = record("book-a", "1", c1);
        let r2 = record("book-a", "2", c2);
        let r3 = record("book-a", "3", c3);
        let stored = vec![r1.clone(), r2.clone(), r3.clone()];

        // Only r1 and r2 were confirmed removed (r3 was unattempted or failed)
        let removed = vec![
            ContactIdentity::new("book-a", "1"),
            ContactIdentity::new("book-a", "2"),
        ];
        let reduced = reduce_contact_delete_result(&stored, &removed);
        assert_eq!(reduced.len(), 1);
        assert_eq!(reduced[0].uid, "3");
    }

    #[test]
    fn contact_selection_action_state_enables_delete_only_for_all_writable() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace", "grace@example.test", "", "");
        let r_writable1 = record("book-a", "1", c1);
        let r_writable2 = record("book-a", "2", c2);
        let r_readonly = record(
            "",
            "3",
            VCardContact::new("Imported", "imp@example.test", "", ""),
        );

        // Empty selection: delete disabled
        let state_empty = contact_selection_action_state(&[], &[]);
        assert!(!state_empty.delete);

        // Multiple writable: delete enabled
        let state_writable =
            contact_selection_action_state(&[r_writable1.clone(), r_writable2.clone()], &[]);
        assert!(state_writable.delete);
        assert_eq!(state_writable.selection_count, 2);

        // Mixed writable + read-only: delete disabled
        let state_mixed = contact_selection_action_state(&[r_writable1, r_readonly], &[]);
        assert!(!state_mixed.delete);
    }

    #[test]
    fn prepare_vcard_for_transfer_replaces_existing_uid() {
        let vcard =
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:old-uid-12345\r\nFN:Ada Lovelace\r\nEND:VCARD";
        let transformed = prepare_vcard_for_transfer(vcard);
        assert!(!transformed.contains("UID:old-uid-12345"));
        assert!(transformed.contains("UID:evolution-gtk4-"));
        assert!(transformed.contains("FN:Ada Lovelace"));
    }

    #[test]
    fn prepare_vcard_for_transfer_inserts_uid_if_missing() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEND:VCARD";
        let transformed = prepare_vcard_for_transfer(vcard);
        assert!(transformed.contains("UID:evolution-gtk4-"));
        assert!(transformed.contains("FN:Ada Lovelace"));
    }

    #[test]
    fn plan_contact_transfer_rejects_empty_selection() {
        let result = plan_contact_transfer(
            &[],
            ContactTransferKind::Copy,
            "target-book",
            "Target Book",
            true,
        );
        assert_eq!(result, Err("Keine Kontakte ausgewählt".to_string()));
    }

    #[test]
    fn plan_contact_transfer_rejects_non_writable_destination() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let r1 = record("book-a", "1", c1);
        let result = plan_contact_transfer(
            &[r1],
            ContactTransferKind::Copy,
            "readonly-book",
            "Readonly Book",
            false,
        );
        assert_eq!(
            result,
            Err("Das Zieladressbuch ist nicht schreibfähig".to_string())
        );
    }

    #[test]
    fn plan_contact_transfer_rejects_same_source_and_destination() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let r1 = record("book-a", "1", c1);
        let result =
            plan_contact_transfer(&[r1], ContactTransferKind::Copy, "book-a", "Book A", true);
        assert_eq!(
            result,
            Err("Quelle und Ziel sind dasselbe Adressbuch".to_string())
        );
    }

    #[test]
    fn plan_contact_transfer_move_rejects_readonly_source() {
        let c1 = VCardContact::new("Imported", "imp@example.test", "", "");
        let r1 = record("", "1", c1);
        let result = plan_contact_transfer(
            &[r1],
            ContactTransferKind::Move,
            "target-book",
            "Target Book",
            true,
        );
        assert_eq!(
            result,
            Err(
                "Mindestens ein ausgewählter Kontakt stammt aus einer schreibgeschützten Quelle"
                    .to_string()
            )
        );
    }

    #[test]
    fn plan_contact_transfer_copy_allows_readonly_source() {
        let c1 = VCardContact::new("Imported", "imp@example.test", "", "");
        let r1 = record("", "1", c1);
        let plan = plan_contact_transfer(
            &[r1],
            ContactTransferKind::Copy,
            "target-book",
            "Target Book",
            true,
        )
        .expect("plan copy succeeds");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.destination_uid, "target-book");
        assert_eq!(plan.kind, ContactTransferKind::Copy);
    }

    #[test]
    fn plan_contact_transfer_rejects_duplicate_identities() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let r1 = record("book-a", "1", c1.clone());
        let r2 = record("book-a", "1", c1);
        let result = plan_contact_transfer(
            &[r1, r2],
            ContactTransferKind::Copy,
            "target-book",
            "Target Book",
            true,
        );
        assert_eq!(
            result,
            Err("Ein ausgewählter Kontakt wurde mehrfach aufgelöst".to_string())
        );
    }

    #[test]
    fn plan_contact_transfer_creates_valid_plan() {
        let c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace Hopper", "grace@example.test", "", "");
        let r1 = record("book-a", "ada-orig", c1);
        let r2 = record("book-a", "grace-orig", c2);
        let plan = plan_contact_transfer(
            &[r1, r2],
            ContactTransferKind::Move,
            "book-b",
            "Book B",
            true,
        )
        .expect("plan move succeeds");
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.kind, ContactTransferKind::Move);
        assert_eq!(plan.destination_uid, "book-b");
        assert_eq!(plan.destination_display_name, "Book B");
        assert_eq!(plan.items[0].name, "Ada Lovelace");
        assert_eq!(plan.items[1].name, "Grace Hopper");
    }

    #[test]
    fn reduce_contact_transfer_result_handles_copy_and_move() {
        let c1 = VCardContact::new("Ada", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace", "grace@example.test", "", "");
        let r1 = record("book-a", "1", c1);
        let r2 = record("book-a", "2", c2);
        let stored = vec![r1.clone(), r2.clone()];

        // Copy: 0 removed from source, 1 created in destination
        let created_record = record(
            "book-b",
            "dest-1",
            VCardContact::new("Ada", "ada@example.test", "", ""),
        );
        let after_copy = reduce_contact_transfer_result(&stored, &[], &[created_record.clone()]);
        assert_eq!(after_copy.len(), 3);

        // Move: 1 removed from source, 1 created in destination
        let removed_identity = ContactIdentity::new("book-a", "1");
        let after_move =
            reduce_contact_transfer_result(&stored, &[removed_identity], &[created_record]);
        assert_eq!(after_move.len(), 2);
        assert_eq!(after_move[0].uid, "2");
        assert_eq!(after_move[1].uid, "dest-1");
    }

    #[test]
    fn ordered_contact_addresses_respects_home_before_work() {
        let mut contact = VCardContact::new("Max Mustermann", "max@example.test", "", "");
        contact.addresses = vec![
            "Arbeitsstraße 10, 10115 Berlin".to_string(),
            "Heimweg 1, 80331 München".to_string(),
        ];
        contact.address_properties = vec![
            crate::services::vcard_service::VCardProperty {
                property: "ADR;TYPE=WORK".to_string(),
                value: "Arbeitsstraße 10, 10115 Berlin".to_string(),
                raw_value: ";;Arbeitsstraße 10;Berlin;;10115;Deutschland".to_string(),
            },
            crate::services::vcard_service::VCardProperty {
                property: "ADR;TYPE=HOME".to_string(),
                value: "Heimweg 1, 80331 München".to_string(),
                raw_value: ";;Heimweg 1;München;;80331;Deutschland".to_string(),
            },
        ];

        // Without home_before_work, natural order is preserved
        let natural = ordered_contact_addresses(&contact, false);
        assert_eq!(natural[0], "Arbeitsstraße 10, 10115 Berlin");
        assert_eq!(natural[1], "Heimweg 1, 80331 München");

        // With home_before_work, HOME is placed first
        let prioritized = ordered_contact_addresses(&contact, true);
        assert_eq!(prioritized[0], "Heimweg 1, 80331 München");
        assert_eq!(prioritized[1], "Arbeitsstraße 10, 10115 Berlin");
    }

    #[test]
    fn contact_detail_summary_with_options_formats_maps_and_lines() {
        let mut contact = VCardContact::new("Max Mustermann", "max@example.test", "", "");
        contact.addresses = vec!["Hauptstraße 1, 10115 Berlin".to_string()];

        let summary_osm_map =
            contact_detail_summary_with_options(&contact, false, true, "openstreetmap", false);
        assert!(summary_osm_map.contains("Adresse: Hauptstraße 1, 10115 Berlin"));
        assert!(summary_osm_map.contains("Karte: aktiv (OpenStreetMap)"));

        let summary_google_map =
            contact_detail_summary_with_options(&contact, false, true, "google", false);
        assert!(summary_google_map.contains("Karte: aktiv (Google Maps)"));

        let summary_no_map =
            contact_detail_summary_with_options(&contact, false, false, "openstreetmap", false);
        assert!(!summary_no_map.contains("Karte:"));

        let mut multi_addr_contact = VCardContact::new("Erika", "erika@example.test", "", "");
        multi_addr_contact.addresses =
            vec!["Straße 1, Berlin".to_string(), "Weg 2, Hamburg".to_string()];
        let formatted = contact_detail_summary_with_options(
            &multi_addr_contact,
            false,
            false,
            "openstreetmap",
            true,
        );
        assert!(formatted.contains("Adresse: Straße 1, Berlin\n         Weg 2, Hamburg"));
    }

    #[test]
    fn test_contacts_paned_divider() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let page = super::ContactsPage::new();
        let paned = page.outer_paned().expect("outer paned exists");

        if let Some(settings) = crate::services::settings_service::AppSettings::try_new() {
            settings.set_mail_sidebar_position(210);
            settings.set_contact_hpane_position(210);

            // Re-sync with position
            paned.set_position(320);
            while context.iteration(false) {}
            assert_eq!(settings.mail_sidebar_position(), 320);
            assert_eq!(settings.contact_hpane_position(), 320);

            // Moving paned must write mail_sidebar_position and contact_hpane_position
            paned.set_position(440);
            while context.iteration(false) {}
            assert_eq!(settings.mail_sidebar_position(), 440);
            assert_eq!(settings.contact_hpane_position(), 440);

            // External change to mail_sidebar_position updates paned
            settings.set_mail_sidebar_position(280);
            while context.iteration(false) {}
            assert_eq!(paned.position(), 280);
        } else {
            assert!(paned.position() >= 80 && paned.position() <= 1500);
        }
    }

    #[test]
    fn test_adversarial_contacts_paned_divider_stress() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let page = super::ContactsPage::new();
        let paned = page.outer_paned().expect("outer paned exists");

        let Some(settings) = crate::services::settings_service::AppSettings::try_new() else {
            return;
        };

        // 1. Boundary & Clamping Stress
        settings.set_mail_sidebar_position(300);
        settings.set_contact_hpane_position(300);
        paned.set_position(300);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 300);
        assert_eq!(settings.contact_hpane_position(), 300);

        // Paned dragging out-of-bounds: < 80 (e.g. 50) must not overwrite settings
        paned.set_position(50);
        while context.iteration(false) {}
        assert_eq!(
            settings.mail_sidebar_position(),
            300,
            "Paned position < 80 must not be written to GSettings"
        );

        // Paned dragging out-of-bounds: > 1500 (e.g. 2000) must not overwrite settings
        paned.set_position(2000);
        while context.iteration(false) {}
        assert_eq!(
            settings.mail_sidebar_position(),
            300,
            "Paned position > 1500 must not be written to GSettings"
        );

        // Valid boundaries: exactly 80 and exactly 1500
        paned.set_position(80);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 80);

        paned.set_position(1500);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 1500);

        // External setting change with large value (e.g. 2500) clamps UI paned to 1500
        settings.set_mail_sidebar_position(2500);
        while context.iteration(false) {}
        assert_eq!(
            paned.position(),
            1500,
            "UI paned must clamp large external GSettings to 1500"
        );

        // External setting change below min (e.g. 40 -> settings clamps to 80)
        settings.set_mail_sidebar_position(40);
        while context.iteration(false) {}
        assert_eq!(
            paned.position(),
            80,
            "UI paned must clamp small external GSettings to 80"
        );

        // 2. Window View Switching & Unified Sidebar Width
        let app = crate::application::EvolutionApplication::new();
        let window = crate::window::EvolutionWindow::new(&app);

        settings.set_mail_sidebar_position(380);
        while context.iteration(false) {}

        // Switching between all views preserves unified sidebar width
        for view in &["contacts", "mail", "calendar", "tasks", "memos", "contacts"] {
            window.set_active_view(view);
            while context.iteration(false) {}
            assert_eq!(
                settings.mail_sidebar_position(),
                380,
                "Switching to view {view} must keep unified mail_sidebar_position"
            );
        }

        // Altering mail-sidebar-position while in mail view syncs across views
        window.set_active_view("mail");
        while context.iteration(false) {}
        settings.set_mail_sidebar_position(490);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 490);

        // Switching back to contacts view: contact paned remains unified
        window.set_active_view("contacts");
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 490);
    }

    #[test]
    fn test_contact_cards_flow_lifecycle() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let flow = gtk::FlowBox::new();
        let mut contact =
            VCardContact::new("Grace Hopper", "grace@example.test", "+49 123 4567", "US Navy");
        contact.title = "Admiral".to_string();
        contact.categories = vec!["Computer Science".to_string()];
        contact.addresses = vec![";;Arlington;VA;;22202;USA".to_string()];

        let rec = record("source-1", "uid-hopper", contact);
        append_contact_card(&flow, &rec);

        let child = find_card_child(&flow, "source-1", "uid-hopper");
        assert!(child.is_some());

        assert!(select_contact_card(&flow, "source-1", "uid-hopper"));
        assert_eq!(flow.selected_children().len(), 1);

        assert!(!select_contact_card(&flow, "source-1", "non-existent"));

        let mut updated_contact =
            VCardContact::new("Grace Hopper", "grace.hopper@navy.mil", "+49 123 4567", "US Navy");
        updated_contact.title = "Rear Admiral".to_string();
        let updated_rec = record("source-1", "uid-hopper", updated_contact);
        upsert_contact_card(&flow, &updated_rec);

        let child_after = find_card_child(&flow, "source-1", "uid-hopper");
        assert!(child_after.is_some());

        assert!(remove_contact_card(&flow, "source-1", "uid-hopper"));
        assert!(find_card_child(&flow, "source-1", "uid-hopper").is_none());
        assert!(!remove_contact_card(&flow, "source-1", "uid-hopper"));
    }

    #[test]
    fn test_contact_card_selection_idempotency_and_sync() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use gtk::prelude::FlowBoxChildExt;
        let flow = gtk::FlowBox::new();
        let contact = VCardContact::new("Alan Turing", "alan@bletchley.test", "+44 1908 12345", "Government");
        let rec = record("source-1", "uid-turing", contact);
        append_contact_card(&flow, &rec);

        let child = find_card_child(&flow, "source-1", "uid-turing").expect("card child exists");
        let card_box = child.child().and_then(|w| w.downcast::<gtk::Box>().ok()).expect("card box exists");
        assert_eq!(card_box.width_request(), 200);
        assert_eq!(card_box.height_request(), 124);

        // First select
        assert!(select_contact_card(&flow, "source-1", "uid-turing"));
        assert!(child.is_selected());

        // Idempotent re-select: should not crash or unselect
        assert!(select_contact_card(&flow, "source-1", "uid-turing"));
        assert!(child.is_selected());

        // Select non-existent
        assert!(!select_contact_card(&flow, "source-1", "missing"));
        assert!(!child.is_selected());
    }

    #[test]
    fn test_contact_card_phones_helper() {
        use crate::services::vcard_service::VCardProperty;

        // 1. Single phone with generic tel
        let mut single_contact = VCardContact::new("Max", "max@test.de", "TestOrg", "Engineer");
        single_contact.tel = "+49 89 12345".to_string();
        let phones = contact_card_phones(&single_contact);
        assert_eq!(phones.len(), 1);
        assert_eq!(phones[0].number, "+49 89 12345");
        assert_eq!(phones[0].label, "");
        assert_eq!(phones[0].icon, "call-start-symbolic");

        // 2. Single mobile phone with TYPE=CELL
        single_contact.phone_properties = vec![VCardProperty {
            property: "TEL;TYPE=CELL".to_string(),
            value: "+49 171 998877".to_string(),
            raw_value: "+49 171 998877".to_string(),
        }];
        single_contact.phones = vec!["+49 171 998877".to_string()];
        let mobile_phones = contact_card_phones(&single_contact);
        assert_eq!(mobile_phones.len(), 1);
        assert_eq!(mobile_phones[0].number, "+49 171 998877");
        assert_eq!(mobile_phones[0].label, "Mobil");
        assert_eq!(mobile_phones[0].icon, "call-start-symbolic");

        // 3. Multiple phones: Work, Home, Cell
        let mut multi_contact = VCardContact::new("Erika", "erika@test.de", "", "WorkOrg");
        multi_contact.phone_properties = vec![
            VCardProperty {
                property: "TEL;TYPE=WORK,VOICE".to_string(),
                value: "+49 89 10001".to_string(),
                raw_value: "+49 89 10001".to_string(),
            },
            VCardProperty {
                property: "TEL;TYPE=HOME,VOICE".to_string(),
                value: "+49 89 20002".to_string(),
                raw_value: "+49 89 20002".to_string(),
            },
            VCardProperty {
                property: "TEL;TYPE=CELL".to_string(),
                value: "+49 170 30003".to_string(),
                raw_value: "+49 170 30003".to_string(),
            },
        ];
        multi_contact.phones = vec![
            "+49 89 10001".to_string(),
            "+49 89 20002".to_string(),
            "+49 170 30003".to_string(),
        ];
        let multi_phones = contact_card_phones(&multi_contact);
        assert_eq!(multi_phones.len(), 3);
        assert_eq!(multi_phones[0].label, "Geschäftlich");
        assert_eq!(multi_phones[0].number, "+49 89 10001");
        assert_eq!(multi_phones[0].icon, "call-start-symbolic");
        assert_eq!(multi_phones[1].label, "Privat");
        assert_eq!(multi_phones[1].number, "+49 89 20002");
        assert_eq!(multi_phones[1].icon, "call-start-symbolic");
        assert_eq!(multi_phones[2].label, "Mobil");
        assert_eq!(multi_phones[2].number, "+49 170 30003");
        assert_eq!(multi_phones[2].icon, "call-start-symbolic");
    }

    #[test]
    fn test_contact_card_multiple_phones_and_dimensions() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use crate::services::vcard_service::VCardProperty;
        let flow = gtk::FlowBox::new();
        let mut contact = VCardContact::new("Ada Lovelace", "ada.lovelace.countess@computing-history.org", "", "Pioneer");
        contact.phone_properties = vec![
            VCardProperty {
                property: "TEL;TYPE=WORK".to_string(),
                value: "+44 20 7946 0991".to_string(),
                raw_value: "+44 20 7946 0991".to_string(),
            },
            VCardProperty {
                property: "TEL;TYPE=HOME".to_string(),
                value: "+44 1483 123456".to_string(),
                raw_value: "+44 1483 123456".to_string(),
            },
        ];
        contact.phones = vec!["+44 20 7946 0991".to_string(), "+44 1483 123456".to_string()];
        let rec = record("source-1", "uid-ada", contact);
        append_contact_card(&flow, &rec);

        let child = find_card_child(&flow, "source-1", "uid-ada").expect("Ada card child exists");
        let card_box = child.child().and_then(|w| w.downcast::<gtk::Box>().ok()).expect("card box exists");
        assert_eq!(card_box.width_request(), 200);
        assert_eq!(card_box.height_request(), 124);
        assert_eq!(child.halign(), gtk::Align::Fill);
        assert_eq!(child.valign(), gtk::Align::Fill);
        assert!(child.hexpands());
        assert_eq!(card_box.halign(), gtk::Align::Fill);
        assert_eq!(card_box.valign(), gtk::Align::Fill);
        assert!(card_box.hexpands());
    }

    #[test]
    fn test_contact_cards_flow_grid_properties() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use glib::subclass::prelude::ObjectSubclassIsExt;
        let page = ContactsPage::new();
        let flow = page.imp().contact_cards_flow.borrow().clone().expect("flow exists");
        assert!(flow.is_homogeneous(), "FlowBox must be homogeneous so all cards have identical format");
        assert_eq!(flow.halign(), gtk::Align::Fill, "FlowBox must have halign Fill so it spans the full viewport width");
        assert!(flow.hexpands(), "FlowBox must hexpand to fill ScrolledWindow");
        assert_eq!(flow.orientation(), gtk::Orientation::Horizontal);
    }

    #[test]
    fn test_flow_allocation_columns() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use glib::subclass::prelude::ObjectSubclassIsExt;
        let page = ContactsPage::new();
        let flow = page.imp().contact_cards_flow.borrow().clone().expect("flow exists");

        let contact1 = VCardContact::new("Willy %", "", "Mobil: +491738334242", "");
        let rec1 = record("s1", "u1", contact1);
        let contact2 = VCardContact::new("Zoë (Cukaff)", "", "Mobil: +4915225739618", "");
        let rec2 = record("s1", "u2", contact2);

        append_contact_card(&flow, &rec1);
        append_contact_card(&flow, &rec2);

        let window = gtk::Window::builder()
            .default_width(1200)
            .default_height(800)
            .child(&page)
            .build();
        window.present();

        for _ in 0..10 {
            _context.iteration(false);
        }

        let mut max_child_min = 0;
        let mut max_child_nat = 0;
        let mut child_count = 0;
        let mut curr = flow.first_child();
        while let Some(c) = curr {
            child_count += 1;
            let (c_min, c_nat, _, _) = c.measure(gtk::Orientation::Horizontal, -1);
            if c_min > max_child_min {
                max_child_min = c_min;
            }
            if c_nat > max_child_nat {
                max_child_nat = c_nat;
            }
            curr = c.next_sibling();
        }

        assert!(child_count > 0, "flow must have children");
        assert!(
            max_child_nat <= 220,
            "max_child_nat ({max_child_nat}) must be <= 220 to allow multi-column layout at standard window widths"
        );

        // At 250px width (narrower than 2 cards, but >= min 226px), exactly 1 column
        let (h_single, _, _, _) = flow.measure(gtk::Orientation::Vertical, 250);

        // At 500px width, must wrap to at least 2 columns (height roughly halved)
        let (h_500, _, _, _) = flow.measure(gtk::Orientation::Vertical, 500);
        assert!(
            h_500 <= (h_single * 6) / 10,
            "At 500px, FlowBox must wrap to 2 columns: h_500={h_500}, h_single={h_single}"
        );

        // At 700px width (user's window size), must wrap to at least 3 columns
        let (h_700, _, _, _) = flow.measure(gtk::Orientation::Vertical, 700);
        assert!(
            h_700 <= (h_single * 4) / 10,
            "At 700px, FlowBox must wrap to at least 3 columns: h_700={h_700}, h_single={h_single}"
        );
    }

    #[test]
    fn test_contacts_drawer_total_and_methods() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use glib::subclass::prelude::ObjectSubclassIsExt;
        let page = ContactsPage::new();
        let drawer_total = page.imp().drawer_total.borrow().clone();
        assert!(drawer_total.is_some(), "drawer_total label should be created");
        let lbl = drawer_total.unwrap();
        assert!(lbl.has_css_class("sidebar-drawer-total"));

        page.update_drawer_total(42);
        assert_eq!(lbl.text().as_str(), "42");
        assert_eq!(lbl.tooltip_text().as_deref(), Some("42 Kontakte insgesamt"));

        page.update_drawer_total(0);
        assert_eq!(lbl.text().as_str(), "");
        assert!(lbl.tooltip_text().is_none());
    }

    #[test]
    fn test_contacts_reload_address_books_and_account_discovery() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use glib::subclass::prelude::ObjectSubclassIsExt;
        let page = ContactsPage::new();
        assert!(
            page.imp().reload_address_books_fn.borrow().is_some(),
            "reload_address_books_fn should be populated"
        );
        page.reload_address_books();
    }

    #[test]
    fn test_contact_table_canvas_layout_and_hit_testing() {
        let mut c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "12345", "Countess");
        c1.org = "Babbage Engines".to_string();
        c1.categories = vec!["Science".to_string(), "Computing".to_string()];
        let r1 = record("book-1", "id-ada", c1);

        let mut c2 = VCardContact::new("Grace Hopper", "grace@navy.mil", "67890", "Admiral");
        c2.org = "US Navy".to_string();
        c2.categories = vec!["Military".to_string()];
        let r2 = record("book-1", "id-grace", c2);

        let mut c3 = VCardContact::new("Alan Turing", "alan@bletchley.uk", "11111", "Mathematician");
        c3.org = "Bletchley Park".to_string();
        c3.categories = vec!["Cryptanalysis".to_string()];
        let r3 = record("book-2", "id-alan", c3);

        let contacts = vec![r1, r2, r3];

        // 1. Sort by Name Ascending
        let mut selected = std::collections::HashSet::new();
        selected.insert(ContactIdentity::new("book-1", "id-grace"));
        let layout_name_asc = prepare_contact_table_layout(
            &contacts,
            None,
            &selected,
            ContactTableColumn::Name,
            false,
            1000.0,
        );
        assert_eq!(layout_name_asc.rows.len(), 3);
        assert_eq!(layout_name_asc.rows[0].name, "Ada Lovelace");
        assert_eq!(layout_name_asc.rows[1].name, "Alan Turing");
        assert_eq!(layout_name_asc.rows[2].name, "Grace Hopper");
        assert!(!layout_name_asc.rows[0].selected);
        assert!(!layout_name_asc.rows[1].selected);
        assert!(layout_name_asc.rows[2].selected);

        // 2. Sort by Name Descending
        let layout_name_desc = prepare_contact_table_layout(
            &contacts,
            None,
            &selected,
            ContactTableColumn::Name,
            true,
            1000.0,
        );
        assert_eq!(layout_name_desc.rows[0].name, "Grace Hopper");
        assert_eq!(layout_name_desc.rows[1].name, "Alan Turing");
        assert_eq!(layout_name_desc.rows[2].name, "Ada Lovelace");

        // 3. Sort by Company Ascending
        let layout_org_asc = prepare_contact_table_layout(
            &contacts,
            None,
            &selected,
            ContactTableColumn::Company,
            false,
            1000.0,
        );
        assert_eq!(layout_org_asc.rows[0].company, "Babbage Engines");
        assert_eq!(layout_org_asc.rows[1].company, "Bletchley Park");
        assert_eq!(layout_org_asc.rows[2].company, "US Navy");

        // 4. Visible Filter
        let mut visible = std::collections::HashSet::new();
        visible.insert(ContactIdentity::new("book-1", "id-ada"));
        visible.insert(ContactIdentity::new("book-2", "id-alan"));
        let layout_filtered = prepare_contact_table_layout(
            &contacts,
            Some(&visible),
            &selected,
            ContactTableColumn::Name,
            false,
            1000.0,
        );
        assert_eq!(layout_filtered.rows.len(), 2);
        assert_eq!(layout_filtered.rows[0].name, "Ada Lovelace");
        assert_eq!(layout_filtered.rows[1].name, "Alan Turing");

        // 5. Hit testing header
        let col_at_10 = hit_test_contact_table_header(&layout_name_asc, 10.0);
        assert_eq!(col_at_10, Some(ContactTableColumn::Name));

        let comp_x = layout_name_asc.col_name_w + 10.0;
        assert_eq!(
            hit_test_contact_table_header(&layout_name_asc, comp_x),
            Some(ContactTableColumn::Company)
        );

        // 6. Hit testing table rows
        assert_eq!(hit_test_contact_table(&layout_name_asc, 5.0), Some(0));
        assert_eq!(hit_test_contact_table(&layout_name_asc, 35.0), Some(1));
        assert_eq!(hit_test_contact_table(&layout_name_asc, 68.0), Some(2));
        assert_eq!(hit_test_contact_table(&layout_name_asc, 500.0), None);
    }

    #[test]
    fn test_contacts_toolbar_and_search_bar_hig() {
        let source = include_str!("contacts_page.rs");
        // Collapsible SearchBar pattern matching Mail and Calendar
        assert!(source.contains("contact_search_bar"));
        assert!(source.contains("search-mode-enabled"));
        assert!(source.contains("connect_stop_search"));
        assert!(source.contains("pub(crate) fn is_search_active"));
        assert!(source.contains("pub(crate) fn close_search"));
        assert!(source.contains("pub(crate) fn search_bar"));
        // Toolbar should not contain search_entry in center_box
        let legacy_center_append = ["contacts_toolbar", ".append(&center", "_box)"].concat();
        assert!(!source.contains(&legacy_center_append));
        assert!(source.contains("content_box.append(&contacts_search_bar)"));

        // Ensure redundant toolbar buttons are removed
        let call_btn_decl = ["let ", "call_btn", " ="].concat();
        let map_btn_decl = ["let ", "map_btn", " ="].concat();
        let web_btn_decl = ["let ", "web_btn", " ="].concat();
        assert!(!source.contains(&call_btn_decl));
        assert!(!source.contains(&map_btn_decl));
        assert!(!source.contains(&web_btn_decl));
        assert!(source.contains("pub(crate) fn sync_toolbar_button_labels"));
        assert!(source.contains("pub(crate) fn apply_toolbar_button_modes"));

        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();
        assert!(page.search_bar().is_some());
        assert!(!page.is_search_active());

        // Test HIG toolbar button modes in desktop mode (pure text, no icon)
        let edit_btn = page.imp().edit_button.borrow().clone().expect("edit button exists");
        let email_btn = page.imp().email_button.borrow().clone().expect("email button exists");
        let delete_btn = page.imp().delete_button.borrow().clone().expect("delete button exists");
        let print_btn = page.imp().print_button.borrow().clone().expect("print button exists");
        let print_list_btn = page.imp().print_list_button.borrow().clone().expect("print_list button exists");
        let export_btn = page.imp().export_button.borrow().clone().expect("export button exists");
        let merge_btn = page.imp().merge_button.borrow().clone().expect("merge button exists");
        let copy_btn = page.imp().copy_to_book_button.borrow().clone().expect("copy button exists");
        let move_btn = page.imp().move_to_book_button.borrow().clone().expect("move button exists");

        let get_label_text = |btn: &gtk::Button| {
            btn.child().and_downcast::<gtk::Label>().map(|l| l.text().to_string())
        };
        let get_image_icon = |btn: &gtk::Button| {
            btn.child().and_downcast::<gtk::Image>().and_then(|img| img.icon_name().map(|s| s.to_string()))
        };

        // Desktop: All buttons have text labels, NO icons
        assert_eq!(get_label_text(&edit_btn), Some("Bearbeiten".to_string()));
        assert_eq!(get_label_text(&email_btn), Some("E-Mail".to_string()));
        assert_eq!(get_label_text(&delete_btn), Some("Löschen".to_string()));
        assert_eq!(get_label_text(&print_btn), Some("Drucken".to_string()));
        assert_eq!(get_label_text(&print_list_btn), Some("Liste drucken".to_string()));
        assert_eq!(get_label_text(&export_btn), Some("Exportieren".to_string()));
        assert_eq!(get_label_text(&merge_btn), Some("Zusammenführen".to_string()));
        assert_eq!(get_label_text(&copy_btn), Some("Kopieren…".to_string()));
        assert_eq!(get_label_text(&move_btn), Some("Verschieben…".to_string()));
        assert_eq!(get_image_icon(&edit_btn), None);

        // Mobile / Compact mode: All buttons switch to pure icons, NO text
        page.set_screen_mode(true, false);
        assert_eq!(get_label_text(&edit_btn), None);
        assert_eq!(get_image_icon(&edit_btn), Some("document-edit-symbolic".to_string()));
        assert_eq!(get_image_icon(&email_btn), Some("mail-message-new-symbolic".to_string()));
        assert_eq!(get_image_icon(&delete_btn), Some("user-trash-symbolic".to_string()));
        assert_eq!(get_image_icon(&print_btn), Some("document-print-symbolic".to_string()));
        assert_eq!(get_image_icon(&print_list_btn), Some("document-print-preview-symbolic".to_string()));
        assert_eq!(get_image_icon(&export_btn), Some("document-save-symbolic".to_string()));
        assert_eq!(get_image_icon(&merge_btn), Some("object-select-symbolic".to_string()));
        assert_eq!(get_image_icon(&copy_btn), Some("edit-copy-symbolic".to_string()));
        assert_eq!(get_image_icon(&move_btn), Some("folder-move-symbolic".to_string()));

        // Restoring desktop mode restores pure text
        page.set_screen_mode(false, false);
        assert_eq!(get_label_text(&edit_btn), Some("Bearbeiten".to_string()));
        assert_eq!(get_label_text(&email_btn), Some("E-Mail".to_string()));
        assert_eq!(get_label_text(&delete_btn), Some("Löschen".to_string()));
        assert_eq!(get_image_icon(&edit_btn), None);

        page.focus_search();
        assert!(page.is_search_active());

        page.close_search();
        assert!(!page.is_search_active());

        page.set_search_query("test-query");
        assert!(page.is_search_active());
        assert_eq!(
            page.imp()
                .search_entry
                .borrow()
                .as_ref()
                .map(|e| e.text().to_string()),
            Some("test-query".to_string())
        );
    }
}
