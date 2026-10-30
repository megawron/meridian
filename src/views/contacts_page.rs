use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// ContactsPage — Address book view with search filtering and dynamic contact detail view.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
#[cfg(target_os = "linux")]
use webkit6::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
use crate::views::accessibility::{
    announce_to_screen_reader, set_accessible_description, set_accessible_label,
    set_accessible_role,
};
use crate::views::set_button_icon_with_fallback;

const LOCAL_ADDRESS_BOOK_SOURCE_UID: &str = "local-addressbook";

#[derive(Debug)]
pub(crate) struct ContactPageSearchState {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum ContactTableColumn {
    #[default]
    Name,
    Company,
    Title,
    Email,
    Phone,
    Categories,
}

impl ContactTableColumn {
    pub(crate) fn id(&self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Company => "company",
            Self::Title => "title",
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Categories => "categories",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "name" => Some(Self::Name),
            "company" => Some(Self::Company),
            "title" => Some(Self::Title),
            "email" => Some(Self::Email),
            "phone" => Some(Self::Phone),
            "categories" => Some(Self::Categories),
            _ => None,
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Name => crate::i18n::gettext("Name"),
            Self::Company => crate::i18n::gettext("Organization"),
            Self::Title => crate::i18n::gettext("Job Title"),
            Self::Email => crate::i18n::gettext("Email"),
            Self::Phone => crate::i18n::gettext("Phone"),
            Self::Categories => crate::i18n::gettext("Categories"),
        }
    }

    pub(crate) fn default_width(&self) -> i32 {
        match self {
            Self::Name => 180,
            Self::Company => 140,
            Self::Title => 120,
            Self::Email => 180,
            Self::Phone => 120,
            Self::Categories => 120,
        }
    }

    pub(crate) fn min_width(&self) -> i32 {
        match self {
            Self::Name => 80,
            Self::Company => 60,
            Self::Title => 60,
            Self::Email => 80,
            Self::Phone => 60,
            Self::Categories => 60,
        }
    }
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
    pub columns: Vec<(ContactTableColumn, f64)>,
    pub col_name_w: f64,
    pub col_company_w: f64,
    pub col_title_w: f64,
    pub col_email_w: f64,
    pub col_phone_w: f64,
    pub col_categories_w: f64,
}

pub(crate) fn clean_contact_org(raw_org: &str) -> String {
    raw_org
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn prepare_contact_table_layout_with_columns(
    contacts: &[EBookContactRecord],
    visible_filter: Option<&HashSet<ContactIdentity>>,
    selected_identities: &HashSet<ContactIdentity>,
    sort_col: ContactTableColumn,
    sort_desc: bool,
    width: f64,
    columns: &[(ContactTableColumn, f64)],
) -> PreparedContactTableLayout {
    let row_height = 34.0;
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
            ContactTableColumn::Company => clean_contact_org(&a.contact.org).cmp(&clean_contact_org(&b.contact.org)),
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
            company: clean_contact_org(&rec.contact.org),
            title: rec.contact.title.clone(),
            email: rec.contact.email.clone(),
            phone: rec.contact.tel.clone(),
            categories: rec.contact.categories.clone(),
            selected,
            y: (i as f64) * row_height,
            height: row_height,
        });
    }

    let mut col_name_w = 0.0;
    let mut col_company_w = 0.0;
    let mut col_title_w = 0.0;
    let mut col_email_w = 0.0;
    let mut col_phone_w = 0.0;
    let mut col_categories_w = 0.0;
    let mut total_cols_w = 0.0;

    for &(col, col_w) in columns {
        total_cols_w += col_w;
        match col {
            ContactTableColumn::Name => col_name_w = col_w,
            ContactTableColumn::Company => col_company_w = col_w,
            ContactTableColumn::Title => col_title_w = col_w,
            ContactTableColumn::Email => col_email_w = col_w,
            ContactTableColumn::Phone => col_phone_w = col_w,
            ContactTableColumn::Categories => col_categories_w = col_w,
        }
    }

    let total_w = total_cols_w.max(width);
    let height = (rows.len() as f64 * row_height).max(40.0);

    PreparedContactTableLayout {
        rows,
        width: total_w,
        height,
        row_height,
        columns: columns.to_vec(),
        col_name_w,
        col_company_w,
        col_title_w,
        col_email_w,
        col_phone_w,
        col_categories_w,
    }
}

pub(crate) fn prepare_contact_table_layout(
    contacts: &[EBookContactRecord],
    visible_filter: Option<&HashSet<ContactIdentity>>,
    selected_identities: &HashSet<ContactIdentity>,
    sort_col: ContactTableColumn,
    sort_desc: bool,
    width: f64,
) -> PreparedContactTableLayout {
    let min_width = 800.0;
    let available_w = width.max(min_width);
    let col_name_w = (available_w * 0.22).max(150.0);
    let col_company_w = (available_w * 0.16).max(120.0);
    let col_title_w = (available_w * 0.14).max(100.0);
    let col_email_w = (available_w * 0.20).max(140.0);
    let col_phone_w = (available_w * 0.14).max(110.0);
    let col_categories_w = (available_w - col_name_w - col_company_w - col_title_w - col_email_w - col_phone_w).max(100.0);
    let default_cols = [
        (ContactTableColumn::Name, col_name_w),
        (ContactTableColumn::Company, col_company_w),
        (ContactTableColumn::Title, col_title_w),
        (ContactTableColumn::Email, col_email_w),
        (ContactTableColumn::Phone, col_phone_w),
        (ContactTableColumn::Categories, col_categories_w),
    ];
    prepare_contact_table_layout_with_columns(
        contacts,
        visible_filter,
        selected_identities,
        sort_col,
        sort_desc,
        available_w,
        &default_cols,
    )
}

pub(crate) fn hit_test_contact_table_header(
    layout: &PreparedContactTableLayout,
    x: f64,
) -> Option<ContactTableColumn> {
    if x < 0.0 || x > layout.width {
        return None;
    }
    let mut cur = 0.0;
    for &(col, col_w) in &layout.columns {
        cur += col_w;
        if x <= cur {
            return Some(col);
        }
    }
    None
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

    let col_name_label = crate::i18n::gettext("Name");
    let col_company_label = crate::i18n::gettext("Organization");
    let col_title_label = crate::i18n::gettext("Job Title");
    let col_email_label = crate::i18n::gettext("Email");
    let col_phone_label = crate::i18n::gettext("Phone");
    let col_categories_label = crate::i18n::gettext("Categories");
    let columns = [
        (ContactTableColumn::Name, col_name_label.as_str(), layout.col_name_w),
        (ContactTableColumn::Company, col_company_label.as_str(), layout.col_company_w),
        (ContactTableColumn::Title, col_title_label.as_str(), layout.col_title_w),
        (ContactTableColumn::Email, col_email_label.as_str(), layout.col_email_w),
        (ContactTableColumn::Phone, col_phone_label.as_str(), layout.col_phone_w),
        (ContactTableColumn::Categories, col_categories_label.as_str(), layout.col_categories_w),
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

mod pangocairo_ffi {
    use gtk::cairo;
    use gtk::glib::translate::{from_glib_full, ToGlibPtr};
    use gtk::pango;

    unsafe extern "C" {
        pub fn pango_cairo_create_layout(
            cr: *mut cairo::ffi::cairo_t,
        ) -> *mut pango::ffi::PangoLayout;
        pub fn pango_cairo_show_layout(
            cr: *mut cairo::ffi::cairo_t,
            layout: *mut pango::ffi::PangoLayout,
        );
        pub fn pango_cairo_update_layout(
            cr: *mut cairo::ffi::cairo_t,
            layout: *mut pango::ffi::PangoLayout,
        );
    }

    pub fn create_layout(cr: &cairo::Context) -> pango::Layout {
        unsafe { from_glib_full(pango_cairo_create_layout(cr.to_raw_none())) }
    }

    pub fn show_layout(cr: &cairo::Context, layout: &pango::Layout) {
        unsafe {
            pango_cairo_show_layout(cr.to_raw_none(), layout.to_glib_none().0);
        }
    }

    pub fn update_layout(cr: &cairo::Context, layout: &pango::Layout) {
        unsafe {
            pango_cairo_update_layout(cr.to_raw_none(), layout.to_glib_none().0);
        }
    }
}

fn draw_contact_avatar_circle(
    cr: &gtk::cairo::Context,
    cx: f64,
    cy: f64,
    r: f64,
    name: &str,
    font_size: f64,
) {
    let (av_r, av_g, av_b) = crate::views::mail_page::avatar_color_for_name(name);
    cr.set_source_rgb(av_r, av_g, av_b);
    cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();

    let initials = crate::views::mail_page::extract_initials(name);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    cr.set_font_size(font_size);
    if let Ok(te) = cr.text_extents(&initials) {
        cr.move_to(
            cx - te.width() / 2.0 - te.x_bearing(),
            cy - te.height() / 2.0 - te.y_bearing(),
        );
    } else {
        cr.move_to(cx - 5.0, cy + 3.5);
    }
    let _ = cr.show_text(&initials);
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

    let (r, g, b) = if is_dark { (0.95, 0.95, 0.94) } else { (0.08, 0.08, 0.08) };
    let (dim_r, dim_g, dim_b) = if is_dark { (0.69, 0.69, 0.67) } else { (0.29, 0.31, 0.32) };
    let (div_r, div_g, div_b) = if is_dark { (1.0, 1.0, 1.0) } else { (0.0, 0.0, 0.0) };

    let font_family = "Adwaita Sans, Cantarell, sans-serif";
    let primary_font_size = 11.0;
    let secondary_font_size = 10.6;

    let mut primary_desc = gtk::pango::FontDescription::new();
    primary_desc.set_family(font_family);
    primary_desc.set_size((primary_font_size * gtk::pango::SCALE as f64).round() as i32);
    primary_desc.set_weight(gtk::pango::Weight::Medium);

    let mut secondary_desc = gtk::pango::FontDescription::new();
    secondary_desc.set_family(font_family);
    secondary_desc.set_size((secondary_font_size * gtk::pango::SCALE as f64).round() as i32);
    secondary_desc.set_weight(gtk::pango::Weight::Normal);

    let mut badge_desc = gtk::pango::FontDescription::new();
    badge_desc.set_family(font_family);
    badge_desc.set_size((9.5 * gtk::pango::SCALE as f64).round() as i32);
    badge_desc.set_weight(gtk::pango::Weight::Medium);

    let pango_layout = pangocairo_ffi::create_layout(cr);
    if let Ok(mut fopts) = gtk::cairo::FontOptions::new() {
        fopts.set_antialias(gtk::cairo::Antialias::Gray);
        fopts.set_hint_style(gtk::cairo::HintStyle::Slight);
        fopts.set_hint_metrics(gtk::cairo::HintMetrics::On);
        cr.set_font_options(&fopts);
        pangocairo_ffi::update_layout(cr, &pango_layout);
    }
    pango_layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
    pango_layout.set_single_paragraph_mode(true);

    let primary_text_h = (primary_font_size * 1.35).round();
    let secondary_text_h = (secondary_font_size * 1.35).round();
    let primary_text_offset = ((layout.row_height - primary_text_h) / 2.0 - 1.5).round();
    let secondary_text_offset = ((layout.row_height - secondary_text_h) / 2.0 - 1.5).round();

    let effective_width = width.max(layout.width);
    let col_count = layout.columns.len();

    for row_idx in start_row..end_row {
        let Some(row) = layout.rows.get(row_idx) else { break; };
        let y = row.y.round();
        let h = row.height.round();

        // 1:1 Mail Table View row background (flat Adwaita selection & hover, no left stripe, no vertical grid lines)
        if row.selected {
            if is_dark {
                cr.set_source_rgba(0.208, 0.518, 0.894, 0.32);
            } else {
                cr.set_source_rgba(0.208, 0.518, 0.894, 0.22);
            }
            cr.rectangle(0.0, y, effective_width, h);
            let _ = cr.fill();
        } else if Some(row_idx) == hovered_row {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            cr.rectangle(0.0, y, effective_width, h);
            let _ = cr.fill();
        }

        if !row.selected {
            cr.set_source_rgba(div_r, div_g, div_b, if is_dark { 0.07 } else { 0.06 });
            cr.set_line_width(1.0);
            cr.move_to(0.0, y + h - 0.5);
            cr.line_to(effective_width, y + h - 0.5);
            let _ = cr.stroke();
        }

        let primary_y = y + primary_text_offset;
        let secondary_y = y + secondary_text_offset;

        let mut col_x = 0.0;
        for (col_idx, &(col, raw_col_w)) in layout.columns.iter().enumerate() {
            let is_last_col = col_idx + 1 == col_count;
            let col_w = if is_last_col {
                (effective_width - col_x).max(raw_col_w)
            } else {
                raw_col_w
            };

            match col {
                ContactTableColumn::Name => {
                    let avatar_r = 10.5;
                    let avatar_cx = (col_x + 8.0 + avatar_r).round();
                    let avatar_cy = (y + h / 2.0).round();
                    let display_name = if row.name.trim().is_empty() {
                        if !row.email.trim().is_empty() {
                            row.email.trim()
                        } else {
                            "—"
                        }
                    } else {
                        row.name.trim()
                    };
                    draw_contact_avatar_circle(cr, avatar_cx, avatar_cy, avatar_r, display_name, 9.0);

                    let text_x = (col_x + 8.0 + avatar_r * 2.0 + 8.0).round();
                    let avail_w = (col_w - (text_x - col_x) - 8.0).max(10.0);
                    pango_layout.set_font_description(Some(&primary_desc));
                    pango_layout.set_width((avail_w * gtk::pango::SCALE as f64) as i32);
                    pango_layout.set_text(display_name);
                    cr.set_source_rgb(r, g, b);
                    cr.move_to(text_x, primary_y);
                    pangocairo_ffi::show_layout(cr, &pango_layout);
                }
                ContactTableColumn::Company => {
                    let text = row.company.trim();
                    if !text.is_empty() {
                        let text_x = (col_x + 6.0).round();
                        let avail_w = (col_w - 12.0).max(10.0);
                        pango_layout.set_font_description(Some(&secondary_desc));
                        pango_layout.set_width((avail_w * gtk::pango::SCALE as f64) as i32);
                        pango_layout.set_text(text);
                        cr.set_source_rgb(dim_r, dim_g, dim_b);
                        cr.move_to(text_x, secondary_y);
                        pangocairo_ffi::show_layout(cr, &pango_layout);
                    }
                }
                ContactTableColumn::Title => {
                    let text = row.title.trim();
                    if !text.is_empty() {
                        let text_x = (col_x + 6.0).round();
                        let avail_w = (col_w - 12.0).max(10.0);
                        pango_layout.set_font_description(Some(&secondary_desc));
                        pango_layout.set_width((avail_w * gtk::pango::SCALE as f64) as i32);
                        pango_layout.set_text(text);
                        cr.set_source_rgb(dim_r, dim_g, dim_b);
                        cr.move_to(text_x, secondary_y);
                        pangocairo_ffi::show_layout(cr, &pango_layout);
                    }
                }
                ContactTableColumn::Email => {
                    let text = row.email.trim();
                    if !text.is_empty() {
                        let text_x = (col_x + 6.0).round();
                        let avail_w = (col_w - 12.0).max(10.0);
                        pango_layout.set_font_description(Some(&secondary_desc));
                        pango_layout.set_width((avail_w * gtk::pango::SCALE as f64) as i32);
                        pango_layout.set_text(text);
                        if is_dark {
                            cr.set_source_rgb(0.47, 0.68, 0.93);
                        } else {
                            cr.set_source_rgb(0.11, 0.44, 0.85);
                        }
                        cr.move_to(text_x, secondary_y);
                        pangocairo_ffi::show_layout(cr, &pango_layout);
                    }
                }
                ContactTableColumn::Phone => {
                    let text = row.phone.trim();
                    if !text.is_empty() {
                        let text_x = (col_x + 6.0).round();
                        let avail_w = (col_w - 12.0).max(10.0);
                        pango_layout.set_font_description(Some(&secondary_desc));
                        pango_layout.set_width((avail_w * gtk::pango::SCALE as f64) as i32);
                        pango_layout.set_text(text);
                        cr.set_source_rgb(dim_r, dim_g, dim_b);
                        cr.move_to(text_x, secondary_y);
                        pangocairo_ffi::show_layout(cr, &pango_layout);
                    }
                }
                ContactTableColumn::Categories => {
                    if !row.categories.is_empty() {
                        let _ = cr.save();
                        cr.rectangle(col_x + 4.0, y, (col_w - 8.0).max(10.0), h);
                        cr.clip();

                        let mut chip_x = (col_x + 6.0).round();
                        let max_x = col_x + col_w - 6.0;
                        pango_layout.set_font_description(Some(&badge_desc));
                        pango_layout.set_width(-1);

                        for cat in row.categories.iter().filter(|c| !c.trim().is_empty()) {
                            let cat_trimmed = cat.trim();
                            pango_layout.set_text(cat_trimmed);
                            let (pw, ph) = pango_layout.pixel_size();
                            let pill_w = (pw as f64 + 12.0).min((max_x - chip_x).max(24.0));
                            let pill_h = (ph as f64 + 4.0).max(18.0);
                            if chip_x + 20.0 > max_x {
                                break;
                            }
                            let pill_y = (y + (h - pill_h) / 2.0).round();
                            let radius = (pill_h / 2.0).min(6.0);
                            let (cr_c, cg_c, cb_c) = crate::views::mail_page::avatar_color_for_name(cat_trimmed);

                            // Draw rounded pill background
                            cr.new_sub_path();
                            cr.arc(chip_x + pill_w - radius, pill_y + radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
                            cr.arc(chip_x + pill_w - radius, pill_y + pill_h - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
                            cr.arc(chip_x + radius, pill_y + pill_h - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
                            cr.arc(chip_x + radius, pill_y + radius, radius, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
                            cr.close_path();

                            cr.set_source_rgba(cr_c, cg_c, cb_c, if is_dark { 0.22 } else { 0.14 });
                            let _ = cr.fill_preserve();
                            cr.set_source_rgba(cr_c, cg_c, cb_c, if is_dark { 0.45 } else { 0.30 });
                            cr.set_line_width(1.0);
                            let _ = cr.stroke();

                            if is_dark {
                                cr.set_source_rgb(
                                    (cr_c * 0.55 + 0.45).min(1.0),
                                    (cg_c * 0.55 + 0.45).min(1.0),
                                    (cb_c * 0.55 + 0.45).min(1.0),
                                );
                            } else {
                                cr.set_source_rgb(cr_c * 0.72, cg_c * 0.72, cb_c * 0.72);
                            }
                            cr.move_to((chip_x + 6.0).round(), (pill_y + (pill_h - ph as f64) / 2.0 - 0.5).round());
                            pangocairo_ffi::show_layout(cr, &pango_layout);

                            chip_x += pill_w + 5.0;
                        }
                        let _ = cr.restore();
                    }
                }
            }
            col_x += col_w;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContactViewMode {
    #[default]
    Simple,
    Table,
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
        pub current_view_mode: Cell<ContactViewMode>,
        pub simple_vpane_pos: Cell<i32>,
        pub table_vpane_pos: Cell<i32>,
        pub ebook_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub contact_search_cancel: RefCell<Option<Arc<AtomicBool>>>,
        pub contact_search_field: Cell<ContactSearchField>,
        pub contact_search_status: RefCell<Option<gtk::Label>>,
        pub contact_search_spinner: RefCell<Option<gtk::Spinner>>,
        pub contact_status_box: RefCell<Option<gtk::Box>>,
        pub contact_search_bar: RefCell<Option<gtk::SearchBar>>,
        pub disposed: Cell<bool>,
        pub selected_identity: RefCell<Option<ContactIdentity>>,
        pub new_contact_button: RefCell<Option<gtk::Button>>,
        pub new_list_button: RefCell<Option<gtk::Button>>,
        pub drawer_total: RefCell<Option<gtk::Label>>,
        pub contact_list: RefCell<Option<gtk::ListBox>>,
        pub contact_table_header: RefCell<Option<gtk::Box>>,
        pub column_header_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub contact_col_widths: RefCell<HashMap<String, i32>>,
        pub contact_table_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub(crate) contact_table_layout: RefCell<Option<PreparedContactTableLayout>>,
        pub contact_table_hovered_row: Cell<Option<usize>>,
        pub(crate) contact_table_hovered_header: Cell<Option<ContactTableColumn>>,
        pub contact_table_anchor_row: Cell<Option<usize>>,
        pub(crate) contact_sort_col: Cell<ContactTableColumn>,
        pub contact_sort_desc: Cell<bool>,
        pub(crate) stored_contacts: RefCell<Option<Rc<RefCell<Vec<EBookContactRecord>>>>>,
        pub(crate) search_state: RefCell<Option<Rc<ContactPageSearchState>>>,
        pub simple_list_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub view_stack: RefCell<Option<gtk::Stack>>,
        pub alphabet_box: RefCell<Option<gtk::ListBox>>,
        pub alphabet_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub list_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub detail_categories_box: RefCell<Option<gtk::Box>>,
        pub phone_group: RefCell<Option<adw::PreferencesGroup>>,
        pub phone_box: RefCell<Option<gtk::Box>>,
        pub details_group: RefCell<Option<adw::PreferencesGroup>>,
        pub details_box: RefCell<Option<gtk::Box>>,
        pub address_group: RefCell<Option<adw::PreferencesGroup>>,
        pub address_box: RefCell<Option<gtk::Box>>,
        pub map_box: RefCell<Option<gtk::Box>>,
        #[cfg(target_os = "linux")]
        pub map_webview: RefCell<Option<webkit6::WebView>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub outer_paned: RefCell<Option<gtk::Paned>>,
        pub reload_fn: RefCell<Option<ReloadContactsCallback>>,
        pub contact_preview_paned: RefCell<Option<gtk::Paned>>,
        pub contacts_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub collapsed_addressbook_groups: RefCell<HashSet<String>>,
        pub is_syncing_selection: Cell<bool>,
        pub toolbar: RefCell<Option<gtk::Widget>>,
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
        pub view_options_split_btn: RefCell<Option<adw::SplitButton>>,
        pub contact_actions_pill: RefCell<Option<gtk::Box>>,
        pub more_button: RefCell<Option<gtk::MenuButton>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContactsPage {
        const NAME: &'static str = "MeridianContactsPage";
        type Type = super::ContactsPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for ContactsPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.simple_vpane_pos.set(380);
            self.table_vpane_pos.set(560);
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
        let canvas_w = self
            .imp()
            .contact_table_canvas
            .borrow()
            .as_ref()
            .map(|c| c.width() as f64)
            .unwrap_or(0.0);
        let w = if canvas_w > 100.0 { canvas_w } else { 600.0 };
        let active_cols = self.active_contact_table_columns(w);
        let layout = prepare_contact_table_layout_with_columns(
            stored_contacts,
            visible_filter,
            selected_identities,
            sort_col,
            sort_desc,
            w,
            &active_cols,
        );
        let content_h = layout.height as i32;
        self.imp().contact_table_layout.replace(Some(layout));
        if let Some(canvas) = self.imp().contact_table_canvas.borrow().as_ref() {
            if canvas.content_height() != content_h {
                canvas.set_content_height(content_h);
            }
        }
        self.queue_contacts_table_draw();
    }

    pub(crate) fn refresh_table_canvas(&self) {
        let stored_opt = self.imp().stored_contacts.borrow().clone();
        let list_opt = self.imp().contact_list.borrow().clone();
        let search_opt = self.imp().search_state.borrow().clone();
        if let (Some(stored), Some(list), Some(search)) = (stored_opt, list_opt, search_opt) {
            let vis = search.visible_identities.borrow();
            let active_query = search.active_query();
            let has_active_filter = !active_query.clauses.is_empty()
                || active_query.filters.source_uid.is_some()
                || active_query.filters.category.is_some()
                || active_query.filters.unmatched_category
                || active_query.filters.duplicate_only;
            let visible_filter = if !has_active_filter {
                None
            } else {
                Some(&*vis)
            };
            let selected = current_contact_selected_identities(&list);
            self.update_contacts_table_layout(&stored.borrow(), visible_filter, &selected);
        }
        self.refresh_contact_column_header();
    }

    pub(crate) fn active_contact_table_columns(&self, available_w: f64) -> Vec<(ContactTableColumn, f64)> {
        let settings = AppSettings::try_new();
        let order = settings
            .as_ref()
            .map(|s| s.contact_table_column_order())
            .unwrap_or_else(|| {
                crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect()
            });

        let mut res = Vec::new();
        for col_id in order {
            let Some(col) = ContactTableColumn::from_id(&col_id) else { continue };
            let visible = settings.as_ref().map(|s| s.contact_col_visible(&col_id)).unwrap_or(true);
            if visible {
                let width = self.get_contact_column_width(&col_id) as f64;
                res.push((col, width));
            }
        }
        if res.is_empty() {
            res.push((ContactTableColumn::Name, 200.0));
        }

        if available_w > 100.0 && !res.is_empty() {
            let total_pref: f64 = res.iter().map(|(_, w)| *w).sum();
            if total_pref > available_w && total_pref > 0.0 {
                let scale = available_w / total_pref;
                for (_, w) in &mut res {
                    *w = (*w * scale).max(36.0).round();
                }
                let scaled_sum: f64 = res.iter().map(|(_, w)| *w).sum();
                if scaled_sum > available_w {
                    let mut excess = scaled_sum - available_w;
                    for (_, w) in res.iter_mut().rev() {
                        if excess <= 0.0 {
                            break;
                        }
                        let reducible = (*w - 32.0).max(0.0);
                        let delta = reducible.min(excess);
                        *w -= delta;
                        excess -= delta;
                    }
                }
            }
            let current_sum: f64 = res.iter().map(|(_, w)| *w).sum();
            if current_sum < available_w {
                if let Some((_, last_w)) = res.last_mut() {
                    *last_w += (available_w - current_sum).max(0.0);
                }
            }
        }

        res
    }

    pub(crate) fn apply_contact_column_width(&self, col_id: &str, width: i32) {
        self.imp().contact_col_widths.borrow_mut().insert(col_id.to_string(), width);
        self.refresh_table_canvas();
    }

    pub(crate) fn resize_contact_column_interactive(
        &self,
        col_id: &str,
        new_width: i32,
        snapshot: &[(String, i32)],
    ) {
        if snapshot.is_empty() {
            self.apply_contact_column_width(col_id, new_width);
            return;
        }
        let Some(idx) = snapshot.iter().position(|(id, _)| id == col_id) else {
            self.apply_contact_column_width(col_id, new_width);
            return;
        };
        let min_self = ContactTableColumn::from_id(col_id)
            .map(|c| c.min_width().min(48))
            .unwrap_or(48);
        let start_w = snapshot[idx].1;
        let mut delta = new_width.max(min_self) - start_w;

        let mut widths: Vec<(String, i32)> = snapshot.to_vec();
        if delta > 0 {
            let mut absorbed = 0;
            for (right_id, right_w) in widths.iter_mut().skip(idx + 1) {
                if delta <= 0 {
                    break;
                }
                let min_r = ContactTableColumn::from_id(right_id)
                    .map(|c| c.min_width().min(48))
                    .unwrap_or(48);
                let can_shrink = (*right_w - min_r).max(0);
                let shrink = can_shrink.min(delta);
                *right_w -= shrink;
                delta -= shrink;
                absorbed += shrink;
            }
            widths[idx].1 = start_w + absorbed;
        } else if delta < 0 {
            let shrink = (-delta).min((start_w - min_self).max(0));
            widths[idx].1 = start_w - shrink;
            if let Some((_, next_w)) = widths.get_mut(idx + 1) {
                *next_w += shrink;
            } else if let Some((_, last_w)) = widths.last_mut() {
                *last_w += shrink;
            }
        }

        {
            let mut map = self.imp().contact_col_widths.borrow_mut();
            for (id, w) in &widths {
                map.insert(id.clone(), *w);
            }
        }
        self.refresh_table_canvas();
    }

    pub(crate) fn snapshot_effective_contact_columns(&self) -> Vec<(String, i32)> {
        let available_w = self
            .imp()
            .contact_table_canvas
            .borrow()
            .as_ref()
            .map(|c| c.width() as f64)
            .unwrap_or(0.0);
        let cols = self.active_contact_table_columns(available_w);
        let snap: Vec<(String, i32)> = cols
            .into_iter()
            .map(|(col, w)| (col.id().to_string(), w.round().max(36.0) as i32))
            .collect();
        {
            let mut map = self.imp().contact_col_widths.borrow_mut();
            for (id, w) in &snap {
                map.insert(id.clone(), *w);
            }
        }
        snap
    }

    fn contact_header_sort_label(&self, col: ContactTableColumn) -> String {
        let base = col.label();
        if self.imp().contact_sort_col.get() != col {
            return base;
        }
        let direction = if self.imp().contact_sort_desc.get() {
            "↓"
        } else {
            "↑"
        };
        format!("{base} {direction}")
    }

    pub(crate) fn get_contact_column_width(&self, col_id: &str) -> i32 {
        if let Some(w) = self.imp().contact_col_widths.borrow().get(col_id) {
            return *w;
        }
        if let Some(settings) = AppSettings::try_new() {
            return settings.contact_col_width(col_id);
        }
        ContactTableColumn::from_id(col_id)
            .map(|c| c.default_width())
            .unwrap_or(120)
    }

    fn contact_column_child(container: &gtk::Box, column_id: &str) -> Option<gtk::Widget> {
        let target_name = format!("contact-column-{column_id}");
        let mut child = container.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == target_name {
                return Some(widget);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn setup_contact_column_header_drag_and_sort(
        &self,
        column_id: String,
        column_widget: &gtk::Box,
        sort_column: ContactTableColumn,
        sort_gesture: &gtk::GestureClick,
        header: &gtk::Box,
    ) {
        let is_dragging = Rc::new(Cell::new(false));
        let start_header_x = Rc::new(Cell::new(0.0f64));
        let drop_target = Rc::new(RefCell::new(None::<(String, bool)>));

        let page_weak_sort = self.downgrade();
        let is_dragging_for_sort = Rc::clone(&is_dragging);
        sort_gesture.connect_released(move |_, _, _, _| {
            if is_dragging_for_sort.get() {
                return;
            }
            if let Some(page) = page_weak_sort.upgrade() {
                let cur_col = page.imp().contact_sort_col.get();
                let cur_desc = page.imp().contact_sort_desc.get();
                let new_desc = if cur_col == sort_column { !cur_desc } else { false };
                page.imp().contact_sort_col.set(sort_column);
                page.imp().contact_sort_desc.set(new_desc);
                page.refresh_contact_column_header();
                page.refresh_table_canvas();
            }
        });

        let drag_gesture = gtk::GestureDrag::new();
        drag_gesture.set_button(1);

        let col_widget_begin = column_widget.clone();
        let header_begin = header.clone();
        let is_dragging_begin = Rc::clone(&is_dragging);
        let start_x_begin = Rc::clone(&start_header_x);
        let drop_target_begin = Rc::clone(&drop_target);

        drag_gesture.connect_drag_begin(move |_, start_x, start_y| {
            is_dragging_begin.set(false);
            *drop_target_begin.borrow_mut() = None;
            let h_x = col_widget_begin
                .compute_point(&header_begin, &gtk::graphene::Point::new(start_x as f32, start_y as f32))
                .map(|p| p.x() as f64)
                .unwrap_or(start_x);
            start_x_begin.set(h_x);
        });

        let col_widget_update = column_widget.clone();
        let header_update = header.clone();
        let is_dragging_update = Rc::clone(&is_dragging);
        let start_x_update = Rc::clone(&start_header_x);
        let drop_target_update = Rc::clone(&drop_target);
        let col_id_update = column_id.clone();

        drag_gesture.connect_drag_update(move |gesture, offset_x, _offset_y| {
            if offset_x.abs() > 6.0 || is_dragging_update.get() {
                is_dragging_update.set(true);
                gesture.set_state(gtk::EventSequenceState::Claimed);
                col_widget_update.add_css_class("column-dragging");
                col_widget_update.set_cursor(gdk::Cursor::from_name("grabbing", None).as_ref());

                let cur_header_x = start_x_update.get() + offset_x;

                let mut columns: Vec<(String, gtk::Widget, f64, f64)> = Vec::new();
                let mut child = header_update.first_child();
                while let Some(w) = child {
                    child = w.next_sibling();
                    let name = w.widget_name().to_string();
                    if name.starts_with("contact-column-") && !name.starts_with("contact-column-sep-") && w.is_visible() {
                        let id = name.trim_start_matches("contact-column-").to_string();
                        if let Some(pt) = w.compute_point(&header_update, &gtk::graphene::Point::new(0.0, 0.0)) {
                            let width = w.width().max(1) as f64;
                            columns.push((id, w.clone(), pt.x() as f64, width));
                        }
                    }
                }

                for (_, w, _, _) in &columns {
                    w.remove_css_class("drop-target-left");
                    w.remove_css_class("drop-target-right");
                }

                let mut target = None;
                if !columns.is_empty() {
                    if cur_header_x < columns[0].2 {
                        let (first_id, first_w, _, _) = &columns[0];
                        if first_id != &col_id_update {
                            first_w.add_css_class("drop-target-left");
                        }
                        target = Some((first_id.clone(), false));
                    } else if cur_header_x > columns.last().unwrap().2 + columns.last().unwrap().3 {
                        let (last_id, last_w, _, _) = columns.last().unwrap();
                        if last_id != &col_id_update {
                            last_w.add_css_class("drop-target-right");
                        }
                        target = Some((last_id.clone(), true));
                    } else {
                        for (id, w, x, width) in &columns {
                            if cur_header_x >= *x && cur_header_x <= *x + *width {
                                let drop_after = cur_header_x >= (*x + *width / 2.0);
                                if id != &col_id_update {
                                    if drop_after {
                                        w.add_css_class("drop-target-right");
                                    } else {
                                        w.add_css_class("drop-target-left");
                                    }
                                }
                                target = Some((id.clone(), drop_after));
                                break;
                            }
                        }
                    }
                }
                *drop_target_update.borrow_mut() = target;
            }
        });

        let col_widget_end = column_widget.clone();
        let header_end = header.clone();
        let is_dragging_end = Rc::clone(&is_dragging);
        let drop_target_end = Rc::clone(&drop_target);
        let page_weak_end = self.downgrade();
        let col_id_end = column_id;

        drag_gesture.connect_drag_end(move |_, _, _| {
            col_widget_end.remove_css_class("column-dragging");
            col_widget_end.set_cursor(None);
            let mut child = header_end.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                w.remove_css_class("drop-target-left");
                w.remove_css_class("drop-target-right");
            }

            if is_dragging_end.get() {
                if let Some((target_id, drop_after)) = drop_target_end.borrow_mut().take() {
                    if target_id != col_id_end {
                        if let Some(settings) = AppSettings::try_new() {
                            let mut order = settings.contact_table_column_order();
                            if let Some(src_pos) = order.iter().position(|id| id == &col_id_end) {
                                let item = order.remove(src_pos);
                                if let Some(tgt_pos) = order.iter().position(|id| id == &target_id) {
                                    let insert_pos = if drop_after { tgt_pos + 1 } else { tgt_pos };
                                    order.insert(insert_pos.min(order.len()), item);
                                    settings.set_contact_table_column_order(&order);
                                    let page_deferred = page_weak_end.clone();
                                    glib::idle_add_local_once(move || {
                                        if let Some(page) = page_deferred.upgrade() {
                                            page.rebuild_contact_column_layout();
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
                let is_dragging_reset = Rc::clone(&is_dragging_end);
                glib::idle_add_local_once(move || {
                    is_dragging_reset.set(false);
                });
            }
        });

        column_widget.add_controller(drag_gesture);
    }

    fn setup_contact_column_resizer(
        &self,
        column_id: String,
        separator: &gtk::Box,
        _column_widget: &gtk::Box,
        _header: &gtk::Box,
    ) {
        let drag_gesture = gtk::GestureDrag::new();
        drag_gesture.set_button(1);

        let width_at_start = Rc::new(Cell::new(0i32));
        let snap_at_start = Rc::new(RefCell::new(Vec::<(String, i32)>::new()));
        let page_begin = self.downgrade();
        let col_id_begin = column_id.clone();
        let w_start_begin = Rc::clone(&width_at_start);
        let snap_begin = Rc::clone(&snap_at_start);

        drag_gesture.connect_drag_begin(move |gesture, _start_x, _start_y| {
            if let Some(page) = page_begin.upgrade() {
                let snap = page.snapshot_effective_contact_columns();
                let cur_w = snap
                    .iter()
                    .find(|(id, _)| id == &col_id_begin)
                    .map(|(_, w)| *w)
                    .unwrap_or_else(|| page.get_contact_column_width(&col_id_begin))
                    .clamp(48, 800);
                w_start_begin.set(cur_w);
                *snap_begin.borrow_mut() = snap;
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });

        let page_update = self.downgrade();
        let col_id_update = column_id.clone();
        let w_start_update = Rc::clone(&width_at_start);
        let snap_update = Rc::clone(&snap_at_start);

        drag_gesture.connect_drag_update(move |gesture, offset_x, _offset_y| {
            if let Some(page) = page_update.upgrade() {
                let min_w = ContactTableColumn::from_id(&col_id_update)
                    .map(|c| c.min_width().min(48))
                    .unwrap_or(48) as f64;
                let new_width = (w_start_update.get() as f64 + offset_x)
                    .round()
                    .clamp(min_w, 800.0) as i32;
                let snap = snap_update.borrow().clone();
                page.resize_contact_column_interactive(&col_id_update, new_width, &snap);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });

        let page_end = self.downgrade();
        let col_id_end = column_id;
        drag_gesture.connect_drag_end(move |_, _, _| {
            if let Some(page) = page_end.upgrade() {
                if let Some(settings) = AppSettings::try_new() {
                    let widths: Vec<(String, i32)> = page
                        .imp()
                        .contact_col_widths
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    for (cid, w) in widths {
                        settings.set_contact_col_width(&cid, w);
                    }
                    let final_w = page.get_contact_column_width(&col_id_end);
                    settings.set_contact_col_width(&col_id_end, final_w);
                }
            }
        });

        separator.add_controller(drag_gesture);
    }

    pub(crate) fn build_contact_column_header(&self) -> gtk::Box {
        let settings = AppSettings::try_new();
        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .margin_start(0)
            .margin_end(0)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["mail-column-header"])
            .build();

        let order = settings
            .as_ref()
            .map(|s| s.contact_table_column_order())
            .unwrap_or_else(|| {
                crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect()
            });

        let last_visible_col_id = order.iter().rfind(|col_id| {
            settings.as_ref().map(|s| s.contact_col_visible(col_id)).unwrap_or(true)
        }).cloned();

        for col_id in &order {
            let Some(col) = ContactTableColumn::from_id(col_id) else { continue };
            let width = settings
                .as_ref()
                .map(|s| s.contact_col_width(col_id))
                .unwrap_or_else(|| col.default_width());
            let visible = settings
                .as_ref()
                .map(|s| s.contact_col_visible(col_id))
                .unwrap_or(true);
            let is_last_visible = last_visible_col_id.as_deref() == Some(col_id.as_str());

            let col_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(0)
                .width_request((width - 1).max(30))
                .hexpand(is_last_visible)
                .halign(gtk::Align::Fill)
                .overflow(gtk::Overflow::Hidden)
                .visible(visible)
                .css_classes(vec!["mail-column-header-cell", "contact-cell"])
                .build();
            col_box.set_widget_name(&format!("contact-column-{col_id}"));

            let label = gtk::Label::builder()
                .label(&self.contact_header_sort_label(col))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .width_chars(1)
                .max_width_chars(1)
                .css_classes(vec!["dim-label"])
                .build();
            label.set_widget_name(&format!("contact-column-label-{col_id}"));
            col_box.append(&label);

            let sort_gesture = gtk::GestureClick::new();
            sort_gesture.set_button(1);
            self.setup_contact_column_header_drag_and_sort(
                col_id.clone(),
                &col_box,
                col,
                &sort_gesture,
                &header,
            );
            col_box.add_controller(sort_gesture);

            let sep = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .width_request(1)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Fill)
                .visible(visible)
                .css_classes(vec!["mail-column-separator"])
                .build();
            sep.set_cursor(gdk::Cursor::from_name("col-resize", None).as_ref());
            sep.set_widget_name(&format!("contact-column-sep-{col_id}"));

            self.setup_contact_column_resizer(col_id.clone(), &sep, &col_box, &header);

            header.append(&col_box);
            header.append(&sep);
        }

        // 10px (±5px) virtual resize grab zone around each 1px separator (1:1 with Mail View)
        let find_hovered_sep = {
            let header_ref = header.clone();
            move |hx: f64| -> Option<(String, gtk::Box)> {
                let mut best: Option<(f64, String, gtk::Box)> = None;
                let mut child = header_ref.first_child();
                while let Some(w) = child {
                    child = w.next_sibling();
                    let name = w.widget_name().to_string();
                    if let Some(col_id) = name.strip_prefix("contact-column-sep-") {
                        if !w.is_visible() {
                            continue;
                        }
                        if let Ok(sep) = w.downcast::<gtk::Box>() {
                            if let Some(pt) = sep.compute_point(&header_ref, &gtk::graphene::Point::new(0.0, 0.0)) {
                                let center_x = pt.x() as f64 + (sep.width().max(1) as f64) * 0.5;
                                let dist = (hx - center_x).abs();
                                if dist <= 5.0 && best.as_ref().is_none_or(|(b_dist, _, _)| dist < *b_dist) {
                                    best = Some((dist, col_id.to_string(), sep));
                                }
                            }
                        }
                    }
                }
                best.map(|(_, id, s)| (id, s))
            }
        };

        let header_motion = gtk::EventControllerMotion::new();
        header_motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let header_for_motion = header.clone();
        let find_for_motion = find_hovered_sep.clone();
        header_motion.connect_motion(move |_, x, _y| {
            let mut child = header_for_motion.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                if w.widget_name().starts_with("contact-column-sep-") {
                    w.remove_css_class("separator-hover");
                }
            }
            if let Some((_, sep)) = find_for_motion(x) {
                sep.add_css_class("separator-hover");
                header_for_motion.set_cursor(gdk::Cursor::from_name("col-resize", None).as_ref());
            } else {
                header_for_motion.set_cursor(None);
            }
        });
        let header_for_leave = header.clone();
        header_motion.connect_leave(move |_| {
            let mut child = header_for_leave.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                if w.widget_name().starts_with("contact-column-sep-") {
                    w.remove_css_class("separator-hover");
                }
            }
            header_for_leave.set_cursor(None);
        });
        header.add_controller(header_motion);

        let header_sep_drag = gtk::GestureDrag::new();
        header_sep_drag.set_button(1);
        header_sep_drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let active_sep_col = Rc::new(RefCell::new(None::<String>));
        let drag_start_w = Rc::new(Cell::new(0i32));
        let drag_snap = Rc::new(RefCell::new(Vec::<(String, i32)>::new()));

        let page_for_hdrag_begin = self.downgrade();
        let find_for_hdrag = find_hovered_sep;
        let active_sep_begin = Rc::clone(&active_sep_col);
        let drag_start_w_begin = Rc::clone(&drag_start_w);
        let drag_snap_begin = Rc::clone(&drag_snap);
        header_sep_drag.connect_drag_begin(move |gesture, start_x, _start_y| {
            let Some(page) = page_for_hdrag_begin.upgrade() else {
                *active_sep_begin.borrow_mut() = None;
                return;
            };
            if let Some((col_id, _)) = find_for_hdrag(start_x) {
                let snap = page.snapshot_effective_contact_columns();
                let cur_w = snap
                    .iter()
                    .find(|(id, _)| id == &col_id)
                    .map(|(_, w)| *w)
                    .unwrap_or_else(|| page.get_contact_column_width(&col_id))
                    .clamp(48, 800);
                drag_start_w_begin.set(cur_w);
                *drag_snap_begin.borrow_mut() = snap;
                *active_sep_begin.borrow_mut() = Some(col_id);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                *active_sep_begin.borrow_mut() = None;
            }
        });

        let page_for_hdrag_update = self.downgrade();
        let active_sep_update = Rc::clone(&active_sep_col);
        let drag_start_w_update = Rc::clone(&drag_start_w);
        let drag_snap_update = Rc::clone(&drag_snap);
        header_sep_drag.connect_drag_update(move |gesture, offset_x, _offset_y| {
            let Some(col_id) = active_sep_update.borrow().clone() else {
                return;
            };
            let Some(page) = page_for_hdrag_update.upgrade() else {
                return;
            };
            let min_w = ContactTableColumn::from_id(&col_id)
                .map(|c| c.min_width().min(48))
                .unwrap_or(48) as f64;
            let new_width = (drag_start_w_update.get() as f64 + offset_x)
                .round()
                .clamp(min_w, 800.0) as i32;
            let snap = drag_snap_update.borrow().clone();
            page.resize_contact_column_interactive(&col_id, new_width, &snap);
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });

        let page_for_hdrag_end = self.downgrade();
        let active_sep_end = Rc::clone(&active_sep_col);
        header_sep_drag.connect_drag_end(move |_, _, _| {
            let Some(col_id) = active_sep_end.borrow_mut().take() else {
                return;
            };
            if let Some(page) = page_for_hdrag_end.upgrade() {
                if let Some(settings) = AppSettings::try_new() {
                    let widths: Vec<(String, i32)> = page
                        .imp()
                        .contact_col_widths
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    for (cid, w) in widths {
                        settings.set_contact_col_width(&cid, w);
                    }
                    let final_w = page.get_contact_column_width(&col_id);
                    settings.set_contact_col_width(&col_id, final_w);
                }
            }
        });
        header.add_controller(header_sep_drag);

        // Right-click context menu gesture
        let header_context_gesture = gtk::GestureClick::new();
        header_context_gesture.set_button(3);
        let page_for_header_context = self.downgrade();
        let header_weak = header.downgrade();
        header_context_gesture.connect_pressed(move |gesture, _, x, y| {
            if let (Some(page), Some(header)) = (page_for_header_context.upgrade(), header_weak.upgrade()) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                page.show_contact_header_context_menu(&header, x, y);
            }
        });
        header.add_controller(header_context_gesture);

        // Long press for touch devices
        let header_long_press = gtk::GestureLongPress::new();
        let page_for_header_lp = self.downgrade();
        let header_weak_lp = header.downgrade();
        header_long_press.connect_pressed(move |gesture, x, y| {
            if let (Some(page), Some(header)) = (page_for_header_lp.upgrade(), header_weak_lp.upgrade()) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                page.show_contact_header_context_menu(&header, x, y);
            }
        });
        header.add_controller(header_long_press);

        header
    }

    fn refresh_contact_column_header(&self) {
        let Some(header) = self.imp().contact_table_header.borrow().as_ref().cloned() else { return; };
        let settings = AppSettings::try_new();

        let order = settings
            .as_ref()
            .map(|s| s.contact_table_column_order())
            .unwrap_or_else(|| {
                crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect()
            });
        let last_visible_col_id = order.iter().rfind(|col_id| {
            settings.as_ref().map(|s| s.contact_col_visible(col_id)).unwrap_or(true)
        }).cloned();

        let available_w = self
            .imp()
            .contact_table_canvas
            .borrow()
            .as_ref()
            .map(|c| c.width() as f64)
            .filter(|w| *w > 100.0)
            .or_else(|| {
                self.imp()
                    .list_scroll
                    .borrow()
                    .as_ref()
                    .map(|s| s.width() as f64)
                    .filter(|w| *w > 100.0)
            })
            .unwrap_or(0.0);
        let effective_cols = self.active_contact_table_columns(available_w);

        for col_id in &crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS {
            let Some(col) = ContactTableColumn::from_id(col_id) else { continue };
            let visible = settings.as_ref().map(|s| s.contact_col_visible(col_id)).unwrap_or(true);
            let width = effective_cols
                .iter()
                .find(|(c, _)| c.id() == *col_id)
                .map(|(_, w)| w.round() as i32)
                .unwrap_or_else(|| self.get_contact_column_width(col_id));
            let is_last_visible = last_visible_col_id.as_deref() == Some(*col_id);

            if let Some(col_box) = Self::contact_column_child(&header, col_id) {
                col_box.set_visible(visible);
                if is_last_visible {
                    col_box.set_width_request(30);
                    col_box.set_hexpand(true);
                } else {
                    col_box.set_width_request((width - 1).max(30));
                    col_box.set_hexpand(false);
                }

                let label_name = format!("contact-column-label-{col_id}");
                let mut child = col_box.first_child();
                while let Some(w) = child {
                    if w.widget_name() == label_name {
                        if let Some(lbl) = w.downcast_ref::<gtk::Label>() {
                            lbl.set_label(&self.contact_header_sort_label(col));
                        }
                        break;
                    }
                    child = w.next_sibling();
                }
            }

            let sep_name = format!("contact-column-sep-{col_id}");
            let mut child = header.first_child();
            while let Some(w) = child {
                if w.widget_name() == sep_name {
                    w.set_visible(visible && !is_last_visible);
                    break;
                }
                child = w.next_sibling();
            }
        }
    }

    fn reorder_contact_column_container(container: &gtk::Box, order: &[String]) {
        let mut children = HashMap::new();
        let mut child = container.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            children.insert(widget.widget_name().to_string(), widget.clone());
            container.remove(&widget);
        }
        for column in order {
            if let Some(widget) = children.remove(&format!("contact-column-{column}")) {
                container.append(&widget);
                if let Some(sep) = children.remove(&format!("contact-column-sep-{column}")) {
                    container.append(&sep);
                }
            }
        }
        for (_, widget) in children {
            container.append(&widget);
        }
    }

    pub(crate) fn rebuild_contact_column_layout(&self) {
        let settings = AppSettings::try_new();
        let order = settings
            .as_ref()
            .map(|s| s.contact_table_column_order())
            .unwrap_or_else(|| {
                crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect()
            });

        if let Some(header) = self.imp().contact_table_header.borrow().as_ref() {
            Self::reorder_contact_column_container(header, &order);
            self.refresh_contact_column_header();
        }
        self.refresh_table_canvas();
    }

    fn show_contact_header_context_menu(&self, header: &gtk::Box, x: f64, y: f64) {
        let popover = gtk::Popover::builder()
            .has_arrow(true)
            .pointing_to(&gdk::Rectangle::new(x as i32, y as i32, 1, 1))
            .build();
        popover.add_css_class("menu");
        popover.add_css_class("context-menu");
        popover.set_parent(header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let title = gtk::Label::builder()
            .label(&crate::i18n::gettext("Columns"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading"])
            .build();
        content.append(&title);

        let settings = AppSettings::try_new();

        let column_toggles: [(&'static str, String); 5] = [
            ("company", crate::i18n::gettext("Organization")),
            ("title", crate::i18n::gettext("Job Title")),
            ("email", crate::i18n::gettext("Email")),
            ("phone", crate::i18n::gettext("Phone")),
            ("categories", crate::i18n::gettext("Categories")),
        ];

        let page_weak = self.downgrade();
        for (key, label) in column_toggles {
            let active = if let Some(ref s) = settings {
                s.contact_col_visible(key)
            } else {
                true
            };

            let check = gtk::CheckButton::builder()
                .label(&label)
                .active(active)
                .build();
            let key_str = key;
            let settings_cloned = settings.clone();
            let page_for_chk = page_weak.clone();
            check.connect_toggled(move |btn| {
                if let Some(ref s) = settings_cloned {
                    s.set_contact_col_visible(key_str, btn.is_active());
                }
                if let Some(page) = page_for_chk.upgrade() {
                    page.rebuild_contact_column_layout();
                }
            });
            content.append(&check);
        }

        content.append(&crate::views::create_context_menu_separator());

        let make_popover_action_button = |_icon_name: &str, label_text: &str| -> gtk::Button {
            crate::views::create_context_menu_button(None, label_text, false)
        };

        let reorder_btn = make_popover_action_button("view-column-symbolic", &crate::i18n::gettext("Customize column order…"));
        let page_for_reorder = page_weak.clone();
        let popover_for_reorder = popover.clone();
        reorder_btn.connect_clicked(move |_| {
            popover_for_reorder.popdown();
            if let Some(page) = page_for_reorder.upgrade() {
                page.open_contact_column_order_dialog();
            }
        });
        content.append(&reorder_btn);

        let reset_btn = make_popover_action_button("edit-undo-symbolic", &crate::i18n::gettext("Restore default columns"));
        let page_for_reset = page_weak.clone();
        let popover_for_reset = popover.clone();
        let settings_for_reset = settings.clone();
        reset_btn.connect_clicked(move |_| {
            if let Some(ref s) = settings_for_reset {
                s.reset_contact_table_columns_to_default();
            }
            if let Some(page) = page_for_reset.upgrade() {
                page.imp().contact_col_widths.borrow_mut().clear();
                page.rebuild_contact_column_layout();
            }
            popover_for_reset.popdown();
        });
        content.append(&reset_btn);

        popover.set_child(Some(&content));
        popover.popup();
    }

    pub(crate) fn open_contact_column_order_dialog(&self) {
        let dialog_title = crate::i18n::gettext("Customize Column Order");
        let dialog = adw::Window::builder()
            .title(&dialog_title)
            .modal(true)
            .default_width(420)
            .default_height(520)
            .build();
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(16)
            .build();

        let desc_label = gtk::Label::builder()
            .label(&crate::i18n::gettext("Adjust the order and visibility of columns. Changes take effect immediately."))
            .wrap(true)
            .xalign(0.0)
            .css_classes(vec!["dim-label", "caption"])
            .build();
        content.append(&desc_label);

        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();

        let settings = AppSettings::try_new();
        let initial_order: Vec<String> = settings
            .as_ref()
            .map(|s| s.contact_table_column_order())
            .unwrap_or_else(|| {
                crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect()
            });

        let order_state = Rc::new(RefCell::new(initial_order));
        let page_weak = self.downgrade();

        fn populate_contact_order_list(
            list: &gtk::ListBox,
            state: &Rc<RefCell<Vec<String>>>,
            settings: &Option<AppSettings>,
            page_weak: &glib::WeakRef<ContactsPage>,
        ) {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let columns = state.borrow().clone();
            for (index, column_id) in columns.iter().enumerate() {
                let Some(col) = ContactTableColumn::from_id(column_id) else { continue };
                let label_text = col.label();
                let row = adw::ActionRow::builder()
                    .title(&label_text)
                    .subtitle(column_id)
                    .build();

                // Visibility switch (except for Name which is mandatory)
                if column_id != "name" {
                    let is_vis = settings.as_ref().map(|s| s.contact_col_visible(column_id)).unwrap_or(true);
                    let vis_check = gtk::CheckButton::builder()
                        .active(is_vis)
                        .tooltip_text(&crate::i18n::gettext("Show column"))
                        .build();
                    let col_id_vis = column_id.clone();
                    let settings_for_vis = settings.clone();
                    let page_for_vis = page_weak.clone();
                    vis_check.connect_toggled(move |btn| {
                        if let Some(ref s) = settings_for_vis {
                            s.set_contact_col_visible(&col_id_vis, btn.is_active());
                        }
                        if let Some(page) = page_for_vis.upgrade() {
                            page.rebuild_contact_column_layout();
                        }
                    });
                    row.add_suffix(&vis_check);
                }

                let up = gtk::Button::builder()
                    .icon_name("go-up-symbolic")
                    .tooltip_text(&crate::i18n::gettext("Move column up / left"))
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index > 0)
                    .build();
                let down = gtk::Button::builder()
                    .icon_name("go-down-symbolic")
                    .tooltip_text(&crate::i18n::gettext("Move column down / right"))
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index + 1 < columns.len())
                    .build();
                row.add_suffix(&up);
                row.add_suffix(&down);

                for (button, target) in [(up, index.checked_sub(1)), (down, Some(index + 1))] {
                    let state_rc = Rc::clone(state);
                    let list_cloned = list.clone();
                    let settings_cloned = settings.clone();
                    let page_weak_cloned = page_weak.clone();
                    button.connect_clicked(move |_| {
                        let Some(target) = target.filter(|target| *target < state_rc.borrow().len()) else {
                            return;
                        };
                        state_rc.borrow_mut().swap(index, target);
                        let order = state_rc.borrow().clone();
                        if let Some(settings) = settings_cloned.as_ref() {
                            settings.set_contact_table_column_order(&order);
                        }
                        if let Some(page) = page_weak_cloned.upgrade() {
                            page.rebuild_contact_column_layout();
                        }
                        populate_contact_order_list(&list_cloned, &state_rc, &settings_cloned, &page_weak_cloned);
                    });
                }
                list.append(&row);
            }
        }

        populate_contact_order_list(&list_box, &order_state, &settings, &page_weak);
        content.append(&list_box);

        let reset_btn = gtk::Button::builder()
            .label(&crate::i18n::gettext("Restore Default Order"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::Center)
            .margin_top(8)
            .build();
        let state_for_reset = Rc::clone(&order_state);
        let list_for_reset = list_box.clone();
        let settings_for_reset = settings.clone();
        let page_for_reset = page_weak.clone();
        reset_btn.connect_clicked(move |_| {
            let defaults: Vec<String> = crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS
                .iter()
                .map(|id| (*id).to_string())
                .collect();
            *state_for_reset.borrow_mut() = defaults.clone();
            if let Some(settings) = settings_for_reset.as_ref() {
                settings.reset_contact_table_columns_to_default();
            }
            if let Some(page) = page_for_reset.upgrade() {
                page.imp().contact_col_widths.borrow_mut().clear();
                page.rebuild_contact_column_layout();
            }
            populate_contact_order_list(&list_for_reset, &state_for_reset, &settings_for_reset, &page_for_reset);
        });
        content.append(&reset_btn);

        scrolled.set_child(Some(&content));
        toolbar.set_content(Some(&scrolled));
        dialog.set_content(Some(&toolbar));
        dialog.present();
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
            button.emit_clicked();
        }
    }

    pub fn present_new_contact_list(&self) {
        if let Some(button) = self.imp().new_list_button.borrow().as_ref() {
            button.emit_clicked();
        }
    }

    pub fn update_drawer_total(&self, total: usize) {
        if let Some(lbl) = self.imp().drawer_total.borrow().as_ref() {
            if total > 0 {
                lbl.set_label(&total.to_string());
                lbl.set_tooltip_text(Some(&crate::i18n::ngettext(
                    "{n} contact in total",
                    "{n} contacts in total",
                    total as u32,
                )
                .replace("{n}", &total.to_string())));
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
        if !self.imp().is_mobile.get() {
            self.imp().is_detail_shown.set(false);
        }
        if let Some(settings) = AppSettings::try_new() {
            if settings.contact_preview_visible() != visible {
                settings.set_contact_preview_visible(visible);
            }
        }
        if let Some(preview_paned) = self.imp().contact_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                detail_box.set_visible(visible);
            }
        }
        self.sync_contacts_layout_visibility();
        self.refresh_table_canvas();
    }

    pub fn is_sidebar_visible(&self) -> bool {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if let Some(start) = paned.start_child() {
                return start.property::<bool>("visible");
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
                    .unwrap_or(240)
                    .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
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
        if !is_mobile {
            imp.is_detail_shown.set(false);
        }
        self.sync_contacts_layout_visibility();
        self.sync_toolbar_button_labels();
    }

    pub(crate) fn apply_toolbar_button_modes(&self, _is_compact: bool) {
        let imp = self.imp();
        let edit_label = crate::i18n::gettext("Edit");
        let email_label = crate::i18n::gettext("Email");
        let delete_label = crate::i18n::gettext("Delete");
        if let Some(btn) = imp.edit_button.borrow().as_ref() {
            btn.set_child(Some(&gtk::Image::from_icon_name("document-edit-symbolic")));
            set_accessible_label(btn, &edit_label);
        }
        if let Some(btn) = imp.email_button.borrow().as_ref() {
            btn.set_child(Some(&gtk::Image::from_icon_name("mail-message-new-symbolic")));
            set_accessible_label(btn, &email_label);
        }
        if let Some(btn) = imp.delete_button.borrow().as_ref() {
            btn.set_child(Some(&gtk::Image::from_icon_name("user-trash-symbolic")));
            set_accessible_label(btn, &delete_label);
        }
    }

    pub(crate) fn sync_toolbar_button_labels(&self) {
        let is_compact = self.imp().is_mobile.get() || (self.width() > 0 && self.width() < 820);
        self.apply_toolbar_button_modes(is_compact);
    }

    pub(crate) fn is_detail_shown(&self) -> bool {
        self.imp().is_detail_shown.get()
    }

    pub(crate) fn selected_contact_display_name(&self) -> Option<String> {
        let imp = self.imp();
        let identity = imp.selected_identity.borrow().clone()?;
        let stored_contacts = imp.stored_contacts.borrow().as_ref().cloned()?;
        let records = stored_contacts.borrow();
        let record = records
            .iter()
            .find(|r| r.source_uid == identity.source_uid && r.uid == identity.uid)?;
        let name = record.contact.fn_name.trim();
        if name.is_empty() {
            if !record.contact.email.trim().is_empty() {
                Some(record.contact.email.trim().to_string())
            } else {
                Some(crate::i18n::gettext("Contact"))
            }
        } else {
            Some(name.to_string())
        }
    }

    pub(crate) fn can_navigate_back(&self) -> bool {
        let imp = self.imp();
        imp.is_mobile.get() && imp.is_detail_shown.get()
    }

    pub(crate) fn navigate_back(&self) -> bool {
        let imp = self.imp();
        if imp.is_detail_shown.get() {
            imp.is_detail_shown.set(false);
            self.sync_contacts_layout_visibility();
            if let Some(win) = crate::window::find_meridian_window(self) {
                win.sync_back_button();
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn sync_contacts_layout_visibility(&self) {
        let imp = self.imp();
        let is_mobile = imp.is_mobile.get();
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
        } else {
            imp.is_detail_shown.set(false);
            let preview_enabled = AppSettings::try_new()
                .map(|s| s.contact_preview_visible())
                .unwrap_or(true);
            let paned_w = preview_paned.width();
            let min_child_w = if paned_w > 0 && paned_w < 440 { 140 } else { 200 };
            if let Some(lc) = list_container.as_ref() {
                lc.set_size_request(min_child_w, -1);
                lc.set_overflow(gtk::Overflow::Hidden);
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_size_request(if preview_enabled { min_child_w } else { 0 }, -1);
                db.set_overflow(gtk::Overflow::Hidden);
                db.set_visible(preview_enabled);
            }
            match imp.current_view_mode.get() {
                ContactViewMode::Table => {
                    if let Some(alpha) = imp.alphabet_scroll.borrow().as_ref() {
                        alpha.set_visible(false);
                    }
                    let mut pos = imp.table_vpane_pos.get().max(340);
                    if preview_enabled {
                        if paned_w >= 440 {
                            let max_pos = (paned_w - 220).max(220);
                            let min_pos = 220.min(max_pos);
                            if pos < min_pos || pos > max_pos {
                                pos = ((paned_w * 58) / 100).clamp(min_pos, max_pos);
                            }
                        } else if paned_w > 0 {
                            pos = (paned_w / 2).max(120);
                        }
                        preview_paned.set_position(pos);
                    }
                }
                ContactViewMode::Simple => {
                    if let Some(alpha) = imp.alphabet_scroll.borrow().as_ref() {
                        alpha.set_visible(true);
                    }
                    let mut pos = imp.simple_vpane_pos.get().max(280);
                    if preview_enabled {
                        if paned_w >= 440 {
                            let max_pos = (paned_w - 220).max(200);
                            let min_pos = 200.min(max_pos);
                            if pos < min_pos || pos > max_pos {
                                pos = ((paned_w * 48) / 100).clamp(min_pos, max_pos);
                            }
                        } else if paned_w > 0 {
                            pos = (paned_w / 2).max(120);
                        }
                        preview_paned.set_position(pos);
                    }
                }
            }
        }

        if let Some(toolbar) = imp.toolbar.borrow().as_ref() {
            if is_mobile {
                toolbar.set_visible(false);
            } else {
                let show_tb = AppSettings::try_new()
                    .map(|s| s.contact_show_toolbar())
                    .unwrap_or(false);
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
            .unwrap_or(false)
    }

    pub fn set_toolbar_visible(&self, visible: bool) {
        if let Some(toolbar) = self.imp().toolbar.borrow().as_ref() {
            toolbar.set_visible(visible);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_contact_show_toolbar(visible);
        }
    }

    pub fn view_options_split_button(&self) -> Option<adw::SplitButton> {
        self.imp().view_options_split_btn.borrow().clone()
    }

    pub fn current_view_mode(&self) -> ContactViewMode {
        self.imp().current_view_mode.get()
    }

    pub fn set_view_mode(&self, mode: ContactViewMode) {
        let imp = self.imp();
        imp.current_view_mode.set(mode);
        imp.is_detail_shown.set(false);
        if let Some(stack) = imp.view_stack.borrow().as_ref() {
            let child_name = match mode {
                ContactViewMode::Simple => "simple",
                ContactViewMode::Table => "table",
            };
            stack.set_visible_child_name(child_name);
        }
        if let Some(settings) = AppSettings::try_new() {
            let layout_str = match mode {
                ContactViewMode::Simple => "vertical",
                ContactViewMode::Table => "classic",
            };
            if settings.contact_layout() != layout_str {
                settings.set_contact_layout(layout_str);
            }
        }
        self.sync_contacts_layout_visibility();
        self.sync_view_split_button();
        if mode == ContactViewMode::Table {
            self.refresh_table_canvas();
        }
        if let Some(win) = crate::window::find_meridian_window(self) {
            win.sync_back_button();
        }
    }

    pub(crate) fn sync_view_split_button(&self) {
        let Some(split_btn) = self.imp().view_options_split_btn.borrow().clone() else {
            return;
        };
        let mode = self.imp().current_view_mode.get();
        let (icon_name, tooltip_text) = match mode {
            ContactViewMode::Simple => ("view-list-symbolic", crate::i18n::gettext("Compact View")),
            ContactViewMode::Table => ("x-office-spreadsheet-symbolic", crate::i18n::gettext("Table View")),
        };
        split_btn.set_icon_name(icon_name);
        split_btn.set_tooltip_text(Some(&tooltip_text));
        set_accessible_label(&split_btn, &tooltip_text);
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
            .css_classes(vec!["contacts-sidebar", "mail-sidebar"])
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(&crate::i18n::gettext("Search contacts…"))
            .build();
        self.imp().search_entry.replace(Some(search_entry.clone()));

        let quick_mode_items = [
            crate::i18n::gettext("All Fields"),
            crate::i18n::gettext("Name"),
            crate::i18n::gettext("Email"),
            crate::i18n::gettext("Phone"),
            crate::i18n::gettext("Category"),
        ];
        let quick_mode_refs: Vec<&str> = quick_mode_items.iter().map(|s| s.as_str()).collect();
        let quick_mode = gtk::DropDown::new(
            Some(gtk::StringList::new(&quick_mode_refs)),
            None::<&gtk::Expression>,
        );
        quick_mode.set_tooltip_text(Some(&crate::i18n::gettext("Select quick search field")));

        let advanced_search_button = gtk::Button::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(&crate::i18n::gettext("Advanced Search…"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();

        let all_contacts_text = crate::i18n::gettext("All Contacts");
        let book_model = gtk::StringList::new(&[&all_contacts_text]);
        let book_selector = gtk::DropDown::new(Some(book_model.clone()), None::<&gtk::Expression>);
        book_selector.set_tooltip_text(Some(&crate::i18n::gettext(
            "The selection will be saved as the default address book for the next launch",
        )));
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

        let all_categories_text = crate::i18n::gettext("All Categories");
        let category_model = gtk::StringList::new(&[&all_categories_text]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some(&crate::i18n::gettext("Select contact category")));
        category_selector.set_margin_bottom(6);

        let selected_book = Rc::new(RefCell::new(String::new()));
        let book_source_uids = Rc::new(RefCell::new(vec![String::new()]));
        let selected_category = Rc::new(RefCell::new(String::new()));
        let duplicate_only = Rc::new(Cell::new(false));
        let unmatched_category = Rc::new(Cell::new(false));

        let duplicate_filter_row = gtk::CheckButton::builder()
            .label(&crate::i18n::gettext("Duplicates only"))
            .tooltip_text(&crate::i18n::gettext("Show same email, phone number, or name without contact path"))
            .halign(gtk::Align::Start)
            .margin_bottom(4)
            .build();

        let unmatched_category_row = gtk::CheckButton::builder()
            .label(&crate::i18n::gettext("Uncategorized only"))
            .tooltip_text(&crate::i18n::gettext("Show contacts without any category"))
            .halign(gtk::Align::Start)
            .margin_bottom(4)
            .build();

        let empty_state = gtk::Label::builder()
            .label(&crate::i18n::gettext("No synchronized address book loaded yet"))
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .visible(false)
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
                    sources.retain(|source| {
                        if !source.enabled {
                            return false;
                        }
                        let parent = source.parent_name.as_deref().unwrap_or("");
                        let fm = source.foreign_mail.as_deref().unwrap_or("");
                        !eds_service::is_shared_or_delegate_mailbox(parent)
                            && !eds_service::is_shared_or_delegate_mailbox(fm)
                            && !eds_service::is_shared_or_delegate_mailbox(&source.uid)
                            && !eds_service::is_shared_or_delegate_mailbox(&source.display_name)
                    });

                    // Canonicalize parent names for discovered sources
                    for s in &mut sources {
                        if s.parent_name.is_none() || s.parent_name.as_deref().unwrap_or("").starts_with("account-contacts-") {
                            let resolved = eds_service::resolve_source_parent_name(&s.uid);
                            if !resolved.is_empty() {
                                s.parent_name = Some(resolved);
                            }
                        }
                    }

                    // Omit GOA redundant wrappers if authoritative books exist for that parent
                    let non_goa_parents: std::collections::HashSet<String> = sources
                        .iter()
                        .filter(|s| !s.uid.starts_with("goa-contacts-"))
                        .filter_map(|s| s.parent_name.as_ref().map(|p| p.to_lowercase()))
                        .collect();
                    sources.retain(|s| {
                        if s.uid.starts_with("goa-contacts-") {
                            let parent = s.parent_name.as_deref().unwrap_or("").to_lowercase();
                            !non_goa_parents.contains(&parent)
                        } else {
                            true
                        }
                    });

                    // Deduplicate sources across caches and backends by (parent_name, canonical_book_name / display_name)
                    let mut deduped_sources: Vec<eds_service::EdsSource> = Vec::new();
                    for s in sources {
                        let p_name = eds_service::canonical_group_name(s.parent_name.as_deref().unwrap_or("")).to_lowercase();
                        let c_kind = eds_service::canonical_book_name(&s.display_name);
                        let is_dup = deduped_sources.iter().any(|existing| {
                            let existing_p = eds_service::canonical_group_name(existing.parent_name.as_deref().unwrap_or("")).to_lowercase();
                            existing.uid == s.uid
                                || (existing_p == p_name
                                    && (!c_kind.is_empty() && c_kind == eds_service::canonical_book_name(&existing.display_name)
                                        || existing.display_name.eq_ignore_ascii_case(&s.display_name)))
                        });
                        if !is_dup {
                            deduped_sources.push(s);
                        }
                    }
                    let mut sources = deduped_sources;

                    sources.sort_by_cached_key(|source| {
                        (source.display_name.to_lowercase(), source.uid.clone())
                    });

                    let mut source_groups = std::collections::HashMap::new();
                    for source in &sources {
                        let raw_group = source
                            .parent_name
                            .clone()
                            .unwrap_or_else(|| eds_service::resolve_source_parent_name(&source.uid));
                        let group = eds_service::canonical_group_name(&raw_group);
                        source_groups.insert(source.uid.clone(), group);
                    }
                    let on_this_computer = eds_service::canonical_local_group_name();
                    source_groups.insert(
                        LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string(),
                        on_this_computer.clone(),
                    );
                    let mut names = vec![crate::i18n::gettext("All Contacts")];
                    let mut uids = vec![String::new()];
                    for source in sources {
                        names.push(contact_source_label(&source.display_name, &source.uid));
                        uids.push(source.uid);
                    }
                    if crate::services::contact_store_service::ContactStoreService::new().is_writable()
                        && !uids.iter().any(|uid| uid == LOCAL_ADDRESS_BOOK_SOURCE_UID)
                    {
                        names.push(crate::i18n::gettext("Local Address Book"));
                        uids.push(LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string());
                    }

                    // If default_source_uid is stale (does not exist in uids), gracefully fallback
                    if !default_source_uid.trim().is_empty() && !uids.iter().any(|uid| uid == &default_source_uid) {
                        let fallback = if uids.iter().any(|uid| uid == LOCAL_ADDRESS_BOOK_SOURCE_UID) {
                            LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string()
                        } else {
                            uids.iter().skip(1).find(|uid| !uid.is_empty()).cloned().unwrap_or_default()
                        };
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_default_address_book_source_uid(&fallback);
                        }
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
                            .unwrap_or_else(|| on_this_computer.clone());
                        if !groups_order.contains(&group) {
                            groups_order.push(group.clone());
                        }
                        group_indices.entry(group).or_default().push(index);
                    }
                    groups_order.sort_by(|a, b| {
                        if a == &on_this_computer {
                            std::cmp::Ordering::Greater
                        } else if b == &on_this_computer {
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
                            let effective_book_uid = if !default_book_uid.trim().is_empty()
                                && uids.iter().any(|uid| uid == &default_book_uid)
                            {
                                default_book_uid
                            } else if uids.iter().any(|uid| uid == LOCAL_ADDRESS_BOOK_SOURCE_UID) {
                                LOCAL_ADDRESS_BOOK_SOURCE_UID.to_string()
                            } else {
                                uids.iter().skip(1).find(|uid| !uid.is_empty()).cloned().unwrap_or_default()
                            };

                            // 1. Root row: "All Contacts"
                            let root_box = gtk::Box::builder()
                                .orientation(gtk::Orientation::Horizontal)
                                .spacing(6)
                                .margin_start(4)
                                .margin_end(6)
                                .valign(gtk::Align::Center)
                                .css_classes(vec![
                                    "calendar-sidebar-row",
                                    "mail-sidebar-row",
                                    "sidebar-child-row",
                                ])
                                .build();
                            let root_icon = gtk::Image::from_icon_name("view-list-symbolic");
                            root_icon.set_pixel_size(16);
                            root_icon.add_css_class("dim-label");
                            root_box.append(&root_icon);
                            let root_label = gtk::Label::builder()
                                .label(&crate::i18n::gettext("All Contacts"))
                                .halign(gtk::Align::Start)
                                .hexpand(true)
                                .ellipsize(pango::EllipsizeMode::End)
                                .build();
                            root_box.append(&root_label);

                            let root_row = gtk::ListBoxRow::builder()
                                .child(&root_box)
                                .css_classes(vec![
                                    "calendar-sidebar-row",
                                    "mail-sidebar-row",
                                    "sidebar-child-row",
                                ])
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
                                group_box.append(&arrow);

                                let group_label = gtk::Label::builder()
                                    .label(group_name)
                                    .halign(gtk::Align::Start)
                                    .hexpand(true)
                                    .ellipsize(pango::EllipsizeMode::End)
                                    .css_classes(vec!["account-heading"])
                                    .build();
                                group_box.append(&group_label);

                                let group_tooltip = if is_collapsed {
                                    crate::i18n::gettext("Expand group")
                                } else {
                                    crate::i18n::gettext("Collapse group")
                                };
                                let group_row = gtk::ListBoxRow::builder()
                                    .child(&group_box)
                                    .activatable(true)
                                    .selectable(false)
                                    .css_classes(vec![
                                        "calendar-sidebar-group-row",
                                        "account-root-row",
                                        "sidebar-account-row",
                                    ])
                                    .tooltip_text(&group_tooltip)
                                    .build();

                                let page_weak_toggle = page.downgrade();
                                let gname = group_name.clone();
                                let group_gesture = gtk::GestureClick::new();
                                let render_holder_click = Rc::clone(&render_holder_for_toggle);
                                let book_selector_click = book_selector.clone();
                                let first_target_idx = indices.first().copied();
                                if let Some(first_idx) = first_target_idx {
                                    unsafe {
                                        group_row.set_data("source-index", first_idx);
                                    }
                                }
                                group_gesture.connect_released(move |_, _, _, _| {
                                    if let Some(first_idx) = first_target_idx {
                                        book_selector_click.set_selected(first_idx as u32);
                                    }
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
                                        .margin_start(14)
                                        .margin_end(4)
                                        .valign(gtk::Align::Center)
                                        .css_classes(vec![
                                            "calendar-sidebar-row",
                                            "mail-sidebar-row",
                                            "calendar-sidebar-child-row",
                                            "sidebar-child-row",
                                        ])
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

                                    let is_primary = !effective_book_uid.is_empty() && effective_book_uid == *source_uid;
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
                                    let render_holder_menu = Rc::clone(&render_holder_for_toggle);
                                    let menu_btn = crate::models::source_selector::create_source_row_menu_full(
                                        is_primary,
                                        &crate::i18n::gettext("Address Book"),
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
                                                if let Some(render_fn) = render_holder_menu.borrow().as_ref() {
                                                    render_fn();
                                                }
                                            }
                                        },
                                        Some({
                                            let uid = source_uid_for_menu.clone();
                                            let name = name_for_menu.clone();
                                            let child_box_weak = child_box_weak.clone();
                                            let child_label = child_label_for_dialog.clone();
                                            move || {
                                                let parent_widget = child_box_weak.upgrade();
                                                let dialog = crate::dialogs::SourcePropertiesDialog::new(
                                                    &uid,
                                                    &name,
                                                    &crate::i18n::gettext("Address Book"),
                                                    "#3584e4",
                                                    true,
                                                    is_primary,
                                                );
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
                                                dialog.present(parent_widget.as_ref());
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
                                                    &gettext("Address Book"),
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
                                        .css_classes(vec![
                                            "calendar-sidebar-row",
                                            "mail-sidebar-row",
                                            "calendar-sidebar-child-row",
                                            "sidebar-child-row",
                                        ])
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
                        uids.iter().position(|uid| uid == &previous_selected_uid).unwrap_or(0)
                    } else {
                        0
                    } as u32;

                    book_selector.set_selected(selected_index);
                    ContactsPage::select_sidebar_source_row(&book_source_list, selected_index as usize);
                    empty_state.set_label(&gettext("{status}; loading contacts.").replace("{status}", &status));
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
            .margin_start(0)
            .margin_end(0)
            .margin_top(0)
            .margin_bottom(0)
            .css_classes(vec!["contact-list-view"])
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
            let render_holder_for_settings = Rc::clone(&render_holder);
            settings.connect_changed(Some("default-address-book-source"), move |_settings, _| {
                if let Some(render_fn) = render_holder_for_settings.borrow().as_ref() {
                    render_fn();
                }
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
                dialog.present(Some(&page));
            }
        ));

        self.imp().stored_contacts.replace(Some(Rc::clone(&stored_contacts)));
        self.imp().search_state.replace(Some(Rc::clone(&search_state)));

        let contact_table_header = self.build_contact_column_header();
        let column_header_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(0)
            .child(&contact_table_header)
            .propagate_natural_width(false)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(vec!["contacts-header-scroll"])
            .build();

        let contact_table_canvas = gtk::DrawingArea::builder()
            .content_height(100)
            .hexpand(true)
            .focusable(true)
            .can_focus(true)
            .focus_on_click(true)
            .css_classes(vec!["contacts-table-canvas"])
            .build();
        set_accessible_role(&contact_table_canvas, gtk::AccessibleRole::List);
        set_accessible_label(&contact_table_canvas, &crate::i18n::gettext("Contacts List"));

        let simple_list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(0)
            .propagate_natural_width(false)
            .child(&contact_list)
            .vexpand(true)
            .hexpand(true)
            .margin_end(28)
            .css_classes(vec!["contact-simple-list-scroll", "contacts-header-scroll"])
            .build();
        self.imp().simple_list_scroll.replace(Some(simple_list_scroll.clone()));

        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(0)
            .propagate_natural_width(false)
            .child(&contact_table_canvas)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.imp().list_scroll.replace(Some(list_scroll.clone()));

        column_header_scroll.set_hadjustment(Some(&list_scroll.hadjustment()));

        self.imp().contact_table_header.replace(Some(contact_table_header.clone()));
        self.imp().column_header_scroll.replace(Some(column_header_scroll.clone()));
        self.imp().contact_table_canvas.replace(Some(contact_table_canvas.clone()));
        contact_list.set_visible(true);

        // Canvas Draw & Resize Func
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
            let page_weak_resize = self.downgrade();
            contact_table_canvas.connect_resize(move |canvas, width, _height| {
                if let Some(page) = page_weak_resize.upgrade() {
                    if !page.imp().is_mobile.get() {
                        if let Some(paned) = page.imp().contact_preview_paned.borrow().as_ref() {
                            let pw = paned.width();
                            let pos = paned.position();
                            if pw >= 440 && (pos < 200 || pos > pw - 200) {
                                page.sync_contacts_layout_visibility();
                            } else if pw > 0 && pw < 440 && (pos < 100 || pos > pw - 100) {
                                page.sync_contacts_layout_visibility();
                            }
                        }
                    }
                    let target_w = (width as f64).max(300.0);
                    let width_changed = page
                        .imp()
                        .contact_table_layout
                        .borrow()
                        .as_ref()
                        .map(|l| (l.width - target_w).abs() > 1.0)
                        .unwrap_or(true);
                    if width_changed {
                        let page_for_idle = page.downgrade();
                        glib::idle_add_local_once(move || {
                            if let Some(p) = page_for_idle.upgrade() {
                                p.refresh_table_canvas();
                            }
                        });
                    }
                }
                canvas.queue_draw();
            });
            let canvas_for_theme = contact_table_canvas.clone();
            let page_for_theme = self.downgrade();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                canvas_for_theme.queue_draw();
                if let Some(page) = page_for_theme.upgrade() {
                    page.refresh_contact_column_header();
                }
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
                    if page.imp().is_mobile.get() {
                        page.imp().is_detail_shown.set(true);
                        page.sync_contacts_layout_visibility();
                        if let Some(win) = crate::window::find_meridian_window(&page) {
                            win.sync_back_button();
                        }
                    } else if let Some(selected_row) = contact_list_clone.selected_row() {
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
                if !shift && !ctrl && page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_contacts_layout_visibility();
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_back_button();
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
            key_controller.connect_key_pressed(move |ctrl, keyval, _keycode, state| {
                let Some(page) = page_weak.upgrade() else { return glib::Propagation::Proceed; };
                let layout_ref = page.imp().contact_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return glib::Propagation::Proceed; };
                if layout.rows.is_empty() {
                    return glib::Propagation::Proceed;
                }

                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let current_anchor = page.imp().contact_table_anchor_row.get().unwrap_or(0).min(layout.rows.len() - 1);

                let announce_row = |row: &PreparedContactTableRow| {
                    if let Some(w) = ctrl.widget() {
                        let speech = if !row.email.is_empty() {
                            format!("{}, {}", row.name, row.email)
                        } else {
                            row.name.clone()
                        };
                        set_accessible_description(&w, &speech);
                        announce_to_screen_reader(&w, &speech, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                };

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
                            announce_row(row);
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
                            announce_row(row);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Page_Up => {
                        let next_idx = current_anchor.saturating_sub(10);
                        page.imp().contact_table_anchor_row.set(Some(next_idx));
                        if let Some(row) = layout.rows.get(next_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                            announce_row(row);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Page_Down => {
                        let next_idx = (current_anchor + 10).min(layout.rows.len() - 1);
                        page.imp().contact_table_anchor_row.set(Some(next_idx));
                        if let Some(row) = layout.rows.get(next_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                            announce_row(row);
                        }
                        scroll_to_row(next_idx);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Home => {
                        page.imp().contact_table_anchor_row.set(Some(0));
                        if let Some(row) = layout.rows.first() {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                            announce_row(row);
                        }
                        scroll_to_row(0);
                        glib::Propagation::Stop
                    }
                    gdk::Key::End => {
                        let last_idx = layout.rows.len().saturating_sub(1);
                        page.imp().contact_table_anchor_row.set(Some(last_idx));
                        if let Some(row) = layout.rows.get(last_idx) {
                            select_contact_row_ext(&contact_list_clone, &row.identity.source_uid, &row.identity.uid, true, false);
                            announce_row(row);
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
            .overflow(gtk::Overflow::Hidden)
            .build();
        contact_table_box.append(&column_header_scroll);
        contact_table_box.append(&list_scroll);

        let view_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .vexpand(true)
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .build();
        view_stack.add_named(&simple_list_scroll, Some("simple"));
        view_stack.add_named(&contact_table_box, Some("table"));
        view_stack.set_visible_child_name("simple");
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
        self.imp().alphabet_scroll.replace(Some(alphabet_scroll.clone()));

        let list_with_alphabet = gtk::Overlay::builder()
            .child(&view_stack)
            .vexpand(true)
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .build();
        list_with_alphabet.add_overlay(&alphabet_scroll);
        list_with_alphabet.set_measure_overlay(&alphabet_scroll, false);

        let list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .vexpand(true)
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .build();

        let status_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(6)
            .margin_bottom(6)
            .visible(false)
            .build();
        status_box.append(&search_spinner);
        status_box.append(&empty_state);

        let update_status_box_visibility = {
            let status_box_weak = status_box.downgrade();
            let spinner_weak = search_spinner.downgrade();
            let empty_weak = empty_state.downgrade();
            move || {
                if let (Some(sb), Some(sp), Some(em)) = (
                    status_box_weak.upgrade(),
                    spinner_weak.upgrade(),
                    empty_weak.upgrade(),
                ) {
                    let is_any_visible = sp.is_visible() || em.is_visible();
                    if sb.is_visible() != is_any_visible {
                        sb.set_visible(is_any_visible);
                    }
                }
            }
        };

        search_spinner.connect_notify_local(
            Some("visible"),
            glib::clone!(
                #[strong]
                update_status_box_visibility,
                move |_, _| {
                    update_status_box_visibility();
                }
            ),
        );
        empty_state.connect_notify_local(
            Some("visible"),
            glib::clone!(
                #[strong]
                update_status_box_visibility,
                move |_, _| {
                    update_status_box_visibility();
                }
            ),
        );
        update_status_box_visibility();

        self.imp()
            .contact_status_box
            .replace(Some(status_box.clone()));

        list_container.append(&status_box);
        list_container.append(&list_with_alphabet);

        let new_contact_btn = gtk::Button::builder()
            .icon_name("contact-new-symbolic")
            .tooltip_text(&crate::i18n::gettext("Create new contact (Ctrl+N)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&new_contact_btn, "contact-new-symbolic", "document-new-symbolic");
        set_accessible_label(&new_contact_btn, &crate::i18n::gettext("New Contact"));

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
                dialog.connect_closed(move |_| {
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
                });
                dialog.present(page.root().as_ref());
            }
        ));
        self.imp()
            .new_contact_button
            .replace(Some(new_contact_btn.clone()));

        let new_list_btn = gtk::Button::builder()
            .icon_name("contact-group-symbolic")
            .tooltip_text(&crate::i18n::gettext("New Contact List"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&new_list_btn, "contact-group-symbolic", "view-list-symbolic");
        set_accessible_label(&new_list_btn, &crate::i18n::gettext("Create new contact list"));
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
                dialog.connect_closed(move |_| {
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
                });
                dialog.present(page.root().as_ref());
            }
        ));
        set_accessible_label(&new_list_btn, &crate::i18n::gettext("Create new contact list"));
        self.imp()
            .new_list_button
            .replace(Some(new_list_btn.clone()));

        let print_list_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Print visible contacts"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&print_list_btn, &crate::i18n::gettext("Print visible contacts"));
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
                        let msg = crate::i18n::ngettext(
                            "Portable list print document prepared ({len} characters, {count} contact); print portal is Linux handoff",
                            "Portable list print document prepared ({len} characters, {count} contacts); print portal is Linux handoff",
                            count as u32,
                        )
                        .replace("{len}", &len.to_string())
                        .replace("{count}", &count.to_string());
                        page.show_toast(&msg);
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
            .label(&crate::i18n::gettext("Contacts"))
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
            .css_classes(vec!["meridian-sidebar-drawer"])
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
            .overflow(gtk::Overflow::Hidden)
            .css_classes(vec!["contact-detail-pane"])
            .build();

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(16)
            .margin_end(16)
            .margin_top(20)
            .margin_bottom(14)
            .halign(gtk::Align::Center)
            .build();

        let detail_avatar = adw::Avatar::builder()
            .size(72)
            .text("")
            .show_initials(true)
            .build();

        let detail_name = gtk::Label::builder()
            .label(&crate::i18n::gettext("No contact selected"))
            .css_classes(vec!["title-1"])
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .build();

        let detail_title = gtk::Label::builder()
            .label(&crate::i18n::gettext("Address book data will be displayed after synchronization"))
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .build();

        let detail_categories_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Center)
            .margin_top(2)
            .visible(false)
            .build();
        self.imp()
            .detail_categories_box
            .replace(Some(detail_categories_box.clone()));

        let detail_extra = gtk::Label::builder()
            .label(&crate::i18n::gettext("No further contact details"))
            .halign(gtk::Align::Start)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .visible(false)
            .css_classes(vec!["body", "dim-label"])
            .build();

        header_box.append(&detail_avatar);
        header_box.append(&detail_name);
        header_box.append(&detail_title);
        header_box.append(&detail_categories_box);

        let contact_actions_pill = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .css_classes(vec!["linked", "mail-action-pill", "contact-header-actions"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(6)
            .visible(false)
            .build();
        self.imp()
            .contact_actions_pill
            .replace(Some(contact_actions_pill.clone()));

        let email_row1 = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("No email address"))
            .subtitle(&crate::i18n::gettext("No contact selected"))
            .build();

        let email_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Send email"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&email_btn, &crate::i18n::gettext("Send email to contact"));
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
                    page.show_toast(&crate::i18n::gettext("The selected contacts have no email address"));
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
            .tooltip_text(&crate::i18n::gettext("Edit contact (Ctrl+E)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&edit_btn, &crate::i18n::gettext("Edit contact"));
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
                    page.show_toast(&crate::i18n::gettext("No contact selected"));
                    return;
                };
                if !contact_source_is_writable(&record.source_uid) {
                    page.show_toast(
                        &crate::i18n::gettext("This contact is from a read-only imported source"),
                    );
                    return;
                }
                let dialog = crate::dialogs::ContactDialog::new_for_edit(&record);
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
                dialog.connect_closed(move |_| {
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
                });
                dialog.present(page.root().as_ref());
            }
        ));

        let merge_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Merge duplicates from same source"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .build();
        set_accessible_label(&merge_btn, &crate::i18n::gettext("Merge duplicates of contact"));
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
                    page.show_toast(&crate::i18n::gettext("No contact selected"));
                    return;
                };
                if !contact_source_is_writable(&primary.source_uid) {
                    page.show_toast(
                        &crate::i18n::gettext("This contact is from a read-only imported source"),
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
                    page.show_toast(&crate::i18n::gettext("No duplicates found for this contact"));
                    merge_btn_for_result.set_sensitive(false);
                    return;
                }

                let count = duplicates.len();
                let confirmation_message = if count == 1 {
                    format!(
                        "{} „{}“ {}. {}",
                        crate::i18n::gettext("1 duplicate will be merged into"),
                        primary.contact.fn_name,
                        crate::i18n::gettext("The primary contact will be kept"),
                        crate::i18n::gettext("The duplicate will then be permanently removed from this address book.")
                    )
                } else {
                    format!(
                        "{count} {} „{}“ {}. {}",
                        crate::i18n::gettext("duplicates will be merged into"),
                        primary.contact.fn_name,
                        crate::i18n::gettext("The primary contact will be kept"),
                        crate::i18n::gettext("The duplicates will then be permanently removed from this address book.")
                    )
                };
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
                    &crate::i18n::gettext("Merge duplicates?"),
                    &confirmation_message,
                    &crate::i18n::gettext("Merge"),
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
                                            &gettext("Merge without confirmed vCard from backend"),
                                        );
                                        return;
                                    };
                                    let Some(contact) =
                                        VCardContact::parse_vcard_string(&vcard)
                                    else {
                                        show_toast_for_widget(
                                            &detail_name_for_action,
                                            &gettext("Could not read merged vCard"),
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
                                        page.update_contact_address_and_map(Some(&merged.contact));
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
                                        page.show_toast(
                                            &gettext("{} duplicate(s) merged into “{}”")
                                                .replacen("{}", &duplicates_for_action.len().to_string(), 1)
                                                .replacen("{}", &merged.contact.fn_name, 1),
                                        );
                                    }
                                }
                                Ok(Err(error)) => show_toast_for_widget(
                                    &detail_name_for_action,
                                    &format!("{}: {error}", gettext("Could not merge duplicates")),
                                ),
                                Err(error) => tracing::debug!(%error, "Contact merge channel closed"),
                            }
                        });
                    },
                );
            }
        ));

        let delete_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Delete contact (Delete)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&delete_btn, &crate::i18n::gettext("Delete contact"));
        self.imp()
            .delete_button
            .replace(Some(delete_btn.clone()));

        let export_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Export selected contacts (vCard, CSV, LDIF)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&export_btn, &crate::i18n::gettext("Export selected contacts"));
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
                    page.show_toast(&crate::i18n::gettext("No contacts selected for export"));
                    return;
                }
                let Some(root) = page.root().and_downcast::<gtk::Window>() else {
                    page.show_toast(&crate::i18n::gettext("No window available for export"));
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
                        "contact.vcf".to_string()
                    } else {
                        format!("{safe}.vcf")
                    }
                } else {
                    "contacts.vcf".to_string()
                };

                let file_dialog = gtk::FileDialog::builder()
                    .title(&crate::i18n::gettext("Export Contacts"))
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
            .tooltip_text(&crate::i18n::gettext("Print selected contacts (Ctrl+P)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&print_btn, &crate::i18n::gettext("Print selected contacts"));
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
                        let label = crate::i18n::ngettext(
                            "Portable print document prepared ({len} characters, {count} contact); print portal is Linux handoff",
                            "Portable print document prepared ({len} characters, {count} contacts); print portal is Linux handoff",
                            count as u32,
                        )
                        .replace("{len}", &len.to_string())
                        .replace("{count}", &count.to_string());
                        page.show_toast(&label);
                    }
                    Err(error) => page.show_toast(&error),
                }
            }
        ));

        let copy_to_book_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Copy to address book…"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&copy_to_book_btn, &crate::i18n::gettext("Copy to Address Book"));
        self.imp()
            .copy_to_book_button
            .replace(Some(copy_to_book_btn.clone()));

        let move_to_book_btn = gtk::Button::builder()
            .tooltip_text(&crate::i18n::gettext("Move to address book…"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&move_to_book_btn, &crate::i18n::gettext("Move to Address Book"));
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
                            page.show_toast(&crate::i18n::gettext("No contact selected"));
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
                            page.show_toast(&crate::i18n::gettext("No contact selected"));
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
                            page.show_toast(&crate::i18n::gettext("No contact selected"));
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
                    &crate::i18n::gettext("Delete"),
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
                                        gettext("The address book source is no longer writable immediately before deletion"),
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
                                                gettext("The backend confirmed deletion of a different contact"),
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
                                                .set_label(&crate::i18n::gettext("No contact selected"));
                                            detail_title_for_result.set_label(
                                                &crate::i18n::gettext("Address book data will be displayed after synchronization"),
                                            );
                                            detail_extra_for_result
                                                .set_label(&crate::i18n::gettext("No further contact details"));
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
                                                .set_label(&crate::i18n::gettext("No contacts loaded in address book"));
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
                                            gettext("{removed_count} contact(s) deleted; error deleting {failed_id}: {error}")
                                                .replace("{removed_count}", &removed_count.to_string())
                                                .replace("{failed_id}", &failed_id.uid)
                                                .replace("{error}", &error)
                                        } else {
                                            format!("{}: {error}", gettext("Could not delete contact"))
                                        };
                                        show_toast_for_widget(&detail_name_for_result, &message);
                                    } else if removed_count == 1 {
                                        let uid = &result.removed[0].uid;
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            page.show_toast(&format!("{}: ({uid})", gettext("Contact deleted")));
                                        }
                                    } else if removed_count > 1 {
                                        if let Some(page) = page_weak_for_result.upgrade() {
                                            page.show_toast(&format!("{removed_count} {}", gettext("contacts deleted")));
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

            let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

            let email = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Send email"),
                false,
            );
            let popover_for_email = popover.clone();
            let email_button_for_menu = email_btn_for_context.clone();
            email.connect_clicked(move |_| {
                popover_for_email.popdown();
                email_button_for_menu.emit_clicked();
            });
            menu_box.append(&email);

            let edit = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Edit contact"),
                false,
            );
            let popover_for_edit = popover.clone();
            let edit_button_for_menu = edit_btn_for_context.clone();
            edit.connect_clicked(move |_| {
                popover_for_edit.popdown();
                edit_button_for_menu.emit_clicked();
            });
            menu_box.append(&edit);

            let export = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Export (vCard, CSV, LDIF)…"),
                false,
            );
            let popover_for_export = popover.clone();
            let export_button_for_menu = export_btn_for_context.clone();
            export.connect_clicked(move |_| {
                popover_for_export.popdown();
                export_button_for_menu.emit_clicked();
            });
            menu_box.append(&export);

            let print = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Print…"),
                false,
            );
            let popover_for_print = popover.clone();
            let print_button_for_menu = print_btn_for_context.clone();
            print.connect_clicked(move |_| {
                popover_for_print.popdown();
                print_button_for_menu.emit_clicked();
            });
            menu_box.append(&print);

            menu_box.append(&crate::views::create_context_menu_separator());

            let copy_to_book = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Copy to address book…"),
                false,
            );
            let popover_for_copy_to_book = popover.clone();
            let copy_to_book_button_for_menu = copy_to_book_btn_for_context.clone();
            copy_to_book.connect_clicked(move |_| {
                popover_for_copy_to_book.popdown();
                copy_to_book_button_for_menu.emit_clicked();
            });
            menu_box.append(&copy_to_book);

            let move_to_book = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Move to address book…"),
                false,
            );
            let popover_for_move_to_book = popover.clone();
            let move_to_book_button_for_menu = move_to_book_btn_for_context.clone();
            move_to_book.connect_clicked(move |_| {
                popover_for_move_to_book.popdown();
                move_to_book_button_for_menu.emit_clicked();
            });
            menu_box.append(&move_to_book);

            let merge = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Merge duplicates"),
                false,
            );
            let popover_for_merge = popover.clone();
            let merge_button_for_menu = merge_btn_for_context.clone();
            merge.connect_clicked(move |_| {
                popover_for_merge.popdown();
                merge_button_for_menu.emit_clicked();
            });
            menu_box.append(&merge);

            menu_box.append(&crate::views::create_context_menu_separator());

            let delete = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Delete contact"),
                true,
            );
            let popover_for_delete = popover.clone();
            let delete_button_for_menu = delete_btn_for_context.clone();
            delete.connect_clicked(move |_| {
                popover_for_delete.popdown();
                delete_button_for_menu.emit_clicked();
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

        let sidebar_visible = AppSettings::try_new()
            .map(|s| s.mail_show_sidebar())
            .unwrap_or(true);
        sidebar_outer_box.set_visible(sidebar_visible);
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle sidebar (F9)"))
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, &crate::i18n::gettext("Toggle sidebar"));
        self.imp()
            .contacts_sidebar_toggle
            .replace(Some(sidebar_toggle.clone()));

        let sidebar_for_toggle = sidebar_outer_box.clone();
        let page_weak_for_sidebar = self.downgrade();
        sidebar_toggle.connect_toggled(move |toggle| {
            let is_active = toggle.is_active();
            if let Some(page) = page_weak_for_sidebar.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&page) {
                    if win.is_sidebar_visible_for_active_view() != is_active {
                        win.toggle_sidebar_animated(is_active);
                        return;
                    }
                }
                page.set_sidebar_visible(is_active);
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_show_sidebar(is_active);
                }
            } else {
                sidebar_for_toggle.set_visible(is_active);
            }
        });

        let list_key_controller = gtk::EventControllerKey::new();
        let delete_btn_for_key = delete_btn.clone();
        let print_btn_for_key = print_btn.clone();
        let print_list_btn_for_key = print_list_btn.clone();
        let contact_list_for_key = contact_list.clone();
        list_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if (key == gtk::gdk::Key::Delete
                || key == gtk::gdk::Key::BackSpace
                || key == gtk::gdk::Key::KP_Delete)
                && modifiers
                    .difference(gtk::gdk::ModifierType::LOCK_MASK)
                    .is_empty()
            {
                if delete_btn_for_key.is_sensitive() {
                    delete_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
            } else if (key == gtk::gdk::Key::a || key == gtk::gdk::Key::A)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                contact_list_for_key.select_all();
                return glib::Propagation::Stop;
            } else if (key == gtk::gdk::Key::p || key == gtk::gdk::Key::P)
                && modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                if print_btn_for_key.is_sensitive() {
                    print_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                } else if print_list_btn_for_key.is_sensitive() {
                    print_list_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        contact_list.add_controller(list_key_controller);

        let page_key_controller = gtk::EventControllerKey::new();
        let sidebar_toggle_for_key = sidebar_toggle.clone();
        page_key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_key.set_active(!sidebar_toggle_for_key.is_active());
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
                        page.sync_contacts_layout_visibility();
                    }
                });
                let page_weak_max = page.downgrade();
                win.connect_maximized_notify(move |_| {
                    if let Some(page) = page_weak_max.upgrade() {
                        glib::idle_add_local_once(move || {
                            page.sync_contacts_layout_visibility();
                            page.refresh_table_canvas();
                        });
                    }
                });
            }
        });

        // --- Setup modern contact action pill in header_box ---
        email_btn.add_css_class("mail-header-action-btn");
        edit_btn.add_css_class("mail-header-action-btn");
        delete_btn.add_css_class("mail-header-action-btn");
        email_btn.set_child(Some(&gtk::Image::from_icon_name("mail-message-new-symbolic")));
        edit_btn.set_child(Some(&gtk::Image::from_icon_name("document-edit-symbolic")));
        delete_btn.set_child(Some(&gtk::Image::from_icon_name("user-trash-symbolic")));

        let more_popover = gtk::Popover::new();
        let more_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .width_request(220)
            .build();

        let make_popover_item = |btn: &gtk::Button, icon: &'static str, label_text: &str| {
            let item_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(10)
                .margin_start(4)
                .margin_end(4)
                .build();
            let img = gtk::Image::from_icon_name(icon);
            let lbl = gtk::Label::builder()
                .label(label_text)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .build();
            item_box.append(&img);
            item_box.append(&lbl);
            btn.set_child(Some(&item_box));
            btn.set_halign(gtk::Align::Fill);
            btn.add_css_class("flat");
            btn.remove_css_class("circular");
        };

        make_popover_item(&print_btn, "document-print-symbolic", &crate::i18n::gettext("Print Contact…"));
        make_popover_item(&export_btn, "document-save-symbolic", &crate::i18n::gettext("Export Contact…"));
        make_popover_item(&merge_btn, "object-select-symbolic", &crate::i18n::gettext("Merge Duplicates…"));
        make_popover_item(&copy_to_book_btn, "edit-copy-symbolic", &crate::i18n::gettext("Copy to Address Book…"));
        make_popover_item(&move_to_book_btn, "folder-move-symbolic", &crate::i18n::gettext("Move to Address Book…"));
        make_popover_item(&print_list_btn, "document-print-preview-symbolic", &crate::i18n::gettext("Print Contact List…"));

        let popover_weak = more_popover.downgrade();
        let connect_popdown = |btn: &gtk::Button, pop_weak: glib::WeakRef<gtk::Popover>| {
            btn.connect_clicked(move |_| {
                if let Some(pop) = pop_weak.upgrade() {
                    pop.popdown();
                }
            });
        };
        connect_popdown(&print_btn, popover_weak.clone());
        connect_popdown(&export_btn, popover_weak.clone());
        connect_popdown(&merge_btn, popover_weak.clone());
        connect_popdown(&copy_to_book_btn, popover_weak.clone());
        connect_popdown(&move_to_book_btn, popover_weak.clone());
        connect_popdown(&print_list_btn, popover_weak.clone());

        more_box.append(&print_btn);
        more_box.append(&export_btn);
        more_box.append(&merge_btn);
        more_box.append(&copy_to_book_btn);
        more_box.append(&move_to_book_btn);
        more_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        more_box.append(&print_list_btn);

        more_popover.set_child(Some(&more_box));

        let str_more_actions = crate::i18n::gettext("More actions for this contact");
        let more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&more_popover)
            .tooltip_text(&str_more_actions)
            .css_classes(vec!["flat", "mail-header-action-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&more_btn, &str_more_actions);
        self.imp().more_button.replace(Some(more_btn.clone()));

        contact_actions_pill.append(&email_btn);
        contact_actions_pill.append(&edit_btn);
        contact_actions_pill.append(&delete_btn);
        contact_actions_pill.append(&more_btn);
        header_box.append(&contact_actions_pill);

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
                if let Some(window) = page.root().and_downcast::<crate::window::MeridianWindow>() {
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

        let popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .width_request(260)
            .build();

        // 1. View Mode Section
        let sec_view_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        sec_view_box.append(&gtk::Label::builder()
            .label(&crate::i18n::gettext("View"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label", "caption"])
            .build()
        );

        let view_mode_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .homogeneous(true)
            .css_classes(vec!["linked"])
            .build();

        let btn_simple = gtk::ToggleButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(&crate::i18n::gettext("Compact View"))
            .active(true)
            .build();
        set_accessible_label(&btn_simple, &crate::i18n::gettext("Compact View"));

        let btn_table = gtk::ToggleButton::builder()
            .icon_name("x-office-spreadsheet-symbolic")
            .tooltip_text(&crate::i18n::gettext("Table View"))
            .group(&btn_simple)
            .build();
        set_accessible_label(&btn_table, &crate::i18n::gettext("Table View"));

        view_mode_box.append(&btn_simple);
        view_mode_box.append(&btn_table);
        sec_view_box.append(&view_mode_box);
        popover_box.append(&sec_view_box);

        let page_weak_s = self.downgrade();
        btn_simple.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(page) = page_weak_s.upgrade() {
                    page.set_view_mode(ContactViewMode::Simple);
                }
            }
        });

        let page_weak_t = self.downgrade();
        btn_table.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(page) = page_weak_t.upgrade() {
                    page.set_view_mode(ContactViewMode::Table);
                }
            }
        });

        // 2. Column Customization Section
        popover_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let btn_cols = gtk::Button::builder()
            .icon_name("view-column-symbolic")
            .label(&crate::i18n::gettext("Customize Column Order…"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::Fill)
            .build();
        set_accessible_label(&btn_cols, &crate::i18n::gettext("Customize Column Order"));

        let options_popover = gtk::Popover::builder().child(&popover_box).build();
        options_popover.add_css_class("view-options-popover");

        let page_for_cols = self.downgrade();
        let pop_for_cols = options_popover.clone();
        btn_cols.connect_clicked(move |_| {
            pop_for_cols.popdown();
            if let Some(page) = page_for_cols.upgrade() {
                page.open_contact_column_order_dialog();
            }
        });
        popover_box.append(&btn_cols);

        // Sync popover toggles on show
        let btn_s = btn_simple.clone();
        let btn_t = btn_table.clone();
        let page_weak_pop = self.downgrade();
        options_popover.connect_show(move |_| {
            if let Some(page) = page_weak_pop.upgrade() {
                match page.imp().current_view_mode.get() {
                    ContactViewMode::Simple => btn_s.set_active(true),
                    ContactViewMode::Table => btn_t.set_active(true),
                }
            }
        });

        // Build SplitButton
        let str_view_opts = crate::i18n::gettext("View options");
        let split_btn = adw::SplitButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text(&crate::i18n::gettext("Compact View"))
            .dropdown_tooltip(&str_view_opts)
            .popover(&options_popover)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&split_btn, &str_view_opts);
        self.imp().view_options_split_btn.replace(Some(split_btn.clone()));

        let page_weak_split = self.downgrade();
        split_btn.connect_clicked(move |_| {
            if let Some(page) = page_weak_split.upgrade() {
                let current = page.imp().current_view_mode.get();
                let next = match current {
                    ContactViewMode::Simple => ContactViewMode::Table,
                    ContactViewMode::Table => ContactViewMode::Simple,
                };
                page.set_view_mode(next);
            }
        });

        self.sync_view_split_button();

        let contacts_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .visible(false)
            .build();
        self.imp()
            .toolbar
            .replace(Some(contacts_toolbar.upcast()));

        detail_box.append(&header_box);
        detail_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let info_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_width(0)
            .propagate_natural_width(false)
            .vexpand(true)
            .css_classes(vec!["contacts-header-scroll"])
            .build();

        let info_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let email_group = adw::PreferencesGroup::builder().title(&crate::i18n::gettext("Email")).build();

        let copy_btn1 = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .tooltip_text(&crate::i18n::gettext("Copy"))
            .build();
        set_accessible_label(&copy_btn1, &crate::i18n::gettext("Copy email address"));
        let selected_contact_for_copy = Rc::clone(&selected_contact);
        let stored_contacts_for_copy = Rc::clone(&stored_contacts);
        copy_btn1.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |button| {
                let Some(record) = selected_contact_for_copy.borrow().clone() else {
                    page.show_toast(&crate::i18n::gettext("No contact selected"));
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
                    page.show_toast(&crate::i18n::gettext("No email address to copy"));
                    return;
                }
                button.display().clipboard().set_text(&email);
                page.show_toast(&crate::i18n::gettext("Email address copied"));
            }
        ));
        email_row1.add_prefix(&gtk::Image::from_icon_name("mail-unread-symbolic"));
        email_row1.add_suffix(&copy_btn1);
        email_group.add(&email_row1);

        let phone_group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Phone"))
            .visible(false)
            .build();
        let phone_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        phone_group.add(&phone_box);
        self.imp().phone_group.replace(Some(phone_group.clone()));
        self.imp().phone_box.replace(Some(phone_box.clone()));

        let address_group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Address"))
            .visible(false)
            .build();

        let address_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        address_group.add(&address_box);

        let map_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .visible(false)
            .build();
        address_group.add(&map_box);

        self.imp().address_group.replace(Some(address_group.clone()));
        self.imp().address_box.replace(Some(address_box.clone()));
        self.imp().map_box.replace(Some(map_box.clone()));

        let details_group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Details"))
            .visible(false)
            .build();
        let details_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        details_group.add(&details_box);
        self.imp().details_group.replace(Some(details_group.clone()));
        self.imp().details_box.replace(Some(details_box.clone()));

        info_box.append(&email_group);
        info_box.append(&phone_group);
        info_box.append(&address_group);
        info_box.append(&details_group);
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
                if let Some(win) = crate::window::find_meridian_window(&page) {
                    win.sync_back_button();
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

        let page_weak_for_activate = self.downgrade();
        contact_list.connect_row_activated(move |_, _| {
            if let Some(page) = page_weak_for_activate.upgrade() {
                if page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_contacts_layout_visibility();
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_back_button();
                    }
                }
            }
        });

        let click_for_mobile_contact = gtk::GestureClick::new();
        let page_for_contact_click = self.downgrade();
        click_for_mobile_contact.connect_released(move |_, _, _, _| {
            if let Some(page) = page_for_contact_click.upgrade() {
                if page.imp().is_mobile.get() && page.imp().selected_identity.borrow().is_some() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_contacts_layout_visibility();
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_back_button();
                    }
                }
            }
        });
        contact_list.add_controller(click_for_mobile_contact);

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
                                     detail_name_for_monitor.set_label(&crate::i18n::gettext("No contact selected"));
                                     detail_title_for_monitor.set_label(
                                         &crate::i18n::gettext("Address book data will be displayed after synchronization"),
                                     );
                                     detail_extra_for_monitor
                                         .set_label(&crate::i18n::gettext("No further contact details"));
                                     email_row_for_monitor.set_title(&crate::i18n::gettext("No email address"));
                                     email_row_for_monitor.set_subtitle(&crate::i18n::gettext("No contact selected"));
                                    page.imp().selected_identity.replace(None);
                                    contact_list_for_monitor.unselect_all();
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
                                    if was_selected {
                                        // Re-select after replacing the row.  This keeps the
                                        // detail pane and keyboard navigation attached to the
                                        // same source UID + contact UID instead of a stale row.
                                        select_contact_row(
                                            &contact_list_for_monitor,
                                            &record.source_uid,
                                            &record.uid,
                                        );
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
                                        page.update_contact_address_and_map(Some(&record.contact));
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

        list_container.set_size_request(200, -1);
        list_container.set_overflow(gtk::Overflow::Hidden);
        detail_box.set_size_request(200, -1);
        detail_box.set_overflow(gtk::Overflow::Hidden);
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
        preview_paned.set_resize_start_child(true);
        preview_paned.set_resize_end_child(true);
        preview_paned.set_shrink_start_child(true);
        preview_paned.set_shrink_end_child(true);

        let stored_pos = AppSettings::try_new()
            .map(|s| s.contact_vpane_position())
            .unwrap_or(380);
        let vpane_pos = stored_pos.clamp(280, 1200);
        self.imp().simple_vpane_pos.set(vpane_pos);
        self.imp().table_vpane_pos.set(vpane_pos.max(460));
        preview_paned.set_position(vpane_pos);
        {
            let page_weak_map = self.downgrade();
            preview_paned.connect_map(move |_| {
                if let Some(page) = page_weak_map.upgrade() {
                    let page_for_idle = page.clone();
                    glib::idle_add_local_once(move || {
                        page_for_idle.sync_contacts_layout_visibility();
                    });
                }
            });
        }
        if let Some(settings) = AppSettings::try_new() {
            let settings_for_notify = settings.clone();
            let page_weak_paned = self.downgrade();
            preview_paned.connect_position_notify(move |paned| {
                let pos = paned.position();
                let pw = paned.width();
                let both_visible = paned.start_child().is_some_and(|c| c.is_visible())
                    && paned.end_child().is_some_and(|c| c.is_visible());
                if both_visible && pw >= 440 && pos >= 200 && pos <= pw - 200 {
                    if let Some(page) = page_weak_paned.upgrade() {
                        match page.imp().current_view_mode.get() {
                            ContactViewMode::Simple => page.imp().simple_vpane_pos.set(pos),
                            ContactViewMode::Table => {
                                page.imp().table_vpane_pos.set(pos);
                                page.refresh_table_canvas();
                            }
                        }
                    }
                    if settings_for_notify.contact_vpane_position() != pos {
                        settings_for_notify.set_contact_vpane_position(pos);
                    }
                }
            });

            let preview_weak = preview_paned.downgrade();
            settings.connect_changed(Some("contact-vpane-position"), move |settings, _| {
                if let Some(paned) = preview_weak.upgrade() {
                    let pos = settings.int("contact-vpane-position").clamp(200, 1400);
                    let pw = paned.width();
                    let clamped = if pw >= 440 {
                        pos.clamp(200, (pw - 200).max(200))
                    } else {
                        pos
                    };
                    if (paned.position() - clamped).abs() > 1 {
                        paned.set_position(clamped);
                    }
                }
            });

            let page_weak_preview_vis = self.downgrade();
            settings.connect_changed(Some("contact-preview-visible"), move |settings, _| {
                if let Some(page) = page_weak_preview_vis.upgrade() {
                    let vis = settings.boolean("contact-preview-visible");
                    if !page.imp().is_mobile.get() {
                        page.imp().is_detail_shown.set(false);
                    }
                    if let Some(paned) = page.imp().contact_preview_paned.borrow().as_ref() {
                        if let Some(db) = paned.end_child() {
                            db.set_visible(vis);
                        }
                    }
                    page.sync_contacts_layout_visibility();
                    page.refresh_table_canvas();
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
        content_box.append(&contacts_search_bar);
        content_box.append(&preview_paned);

        paned.set_end_child(Some(&content_box));
        paned.set_resize_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(true);

        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(240)
            .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
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
                let clamped = position.clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                if position != clamped {
                    paned.set_position(clamped);
                }
                if let Some(win) = crate::window::find_meridian_window(paned) {
                    win.set_sidebar_position_sync(clamped);
                }
                if settings_for_notify.mail_sidebar_position() != clamped {
                    settings_for_notify.set_mail_sidebar_position(clamped);
                }
                if settings_for_notify.contact_hpane_position() != clamped {
                    settings_for_notify.set_contact_hpane_position(clamped);
                }
            });

            let paned_weak = paned.downgrade();
            settings.connect_changed(Some("mail-sidebar-position"), move |settings, _| {
                if let Some(paned) = paned_weak.upgrade() {
                    let pos = settings.int("mail-sidebar-position").clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                    if (paned.position() - pos).abs() > 1 {
                        paned.set_position(pos);
                    }
                }
            });

            let paned_weak_hpane = paned.downgrade();
            settings.connect_changed(Some("contact-hpane-position"), move |settings, _| {
                if let Some(paned) = paned_weak_hpane.upgrade() {
                    let pos = settings.int("contact-hpane-position").clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                    if (paned.position() - pos).abs() > 1 {
                        paned.set_position(pos);
                    }
                }
            });
        }

        if let Some(settings) = AppSettings::try_new() {
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

            let page_weak_home = self.downgrade();
            let detail_extra_for_home = detail_extra.clone();
            let selected_for_home = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-preview-home-before-work"), move |_, _| {
                if let Some(record) = selected_for_home.borrow().as_ref() {
                    detail_extra_for_home.set_label(&contact_detail_summary(&record.contact));
                    if let Some(page) = page_weak_home.upgrade() {
                        page.update_contact_address_and_map(Some(&record.contact));
                    }
                }
            });

            let page_weak_maps = self.downgrade();
            let detail_extra_for_maps = detail_extra.clone();
            let selected_for_maps = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-preview-show-maps"), move |_, _| {
                if let Some(record) = selected_for_maps.borrow().as_ref() {
                    detail_extra_for_maps.set_label(&contact_detail_summary(&record.contact));
                    if let Some(page) = page_weak_maps.upgrade() {
                        page.update_contact_address_and_map(Some(&record.contact));
                    }
                }
            });

            let page_weak_formatting = self.downgrade();
            let detail_extra_for_formatting = detail_extra.clone();
            let selected_for_formatting = Rc::clone(&selected_contact);
            settings.connect_changed(Some("contact-address-formatting"), move |_, _| {
                if let Some(record) = selected_for_formatting.borrow().as_ref() {
                    detail_extra_for_formatting.set_label(&contact_detail_summary(&record.contact));
                    if let Some(page) = page_weak_formatting.upgrade() {
                        page.update_contact_address_and_map(Some(&record.contact));
                    }
                }
            });
        }

        let initial_layout = AppSettings::try_new()
            .map(|s| s.contact_layout())
            .unwrap_or_else(|| "vertical".to_string());
        let initial_mode = if initial_layout == "classic" {
            ContactViewMode::Table
        } else {
            ContactViewMode::Simple
        };
        self.set_view_mode(initial_mode);

        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        root_box.append(&paned);
        root_box.set_parent(self);
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Contact page result");
    }

    pub(crate) fn update_contact_address_and_map(&self, contact: Option<&VCardContact>) {
        let Some(group) = self.imp().address_group.borrow().clone() else { return; };
        let Some(addr_box) = self.imp().address_box.borrow().clone() else { return; };
        let Some(map_box) = self.imp().map_box.borrow().clone() else { return; };

        while let Some(child) = addr_box.first_child() {
            addr_box.remove(&child);
        }

        let Some(contact) = contact else {
            group.set_visible(false);
            map_box.set_visible(false);
            return;
        };

        let home_before_work = AppSettings::try_new()
            .map(|s| s.contact_preview_home_before_work())
            .unwrap_or(false);
        let show_maps = AppSettings::try_new()
            .map(|s| s.contact_preview_show_maps())
            .unwrap_or(true);
        let target = AppSettings::try_new()
            .map(|s| s.contact_open_map_target())
            .unwrap_or_else(|| "openstreetmap".to_string());
        let prefer_local = AppSettings::try_new()
            .map(|s| s.contact_open_map_prefer_local())
            .unwrap_or(true);

        let ordered_addrs = ordered_contact_addresses(contact, home_before_work);
        if ordered_addrs.is_empty() {
            group.set_visible(false);
            map_box.set_visible(false);
            return;
        }

        group.set_visible(true);

        for addr in &ordered_addrs {
            let row = adw::ActionRow::builder()
                .title(addr)
                .selectable(false)
                .css_classes(vec!["property"])
                .build();
            let prefix_icon = gtk::Image::from_icon_name("mark-location-symbolic");
            row.add_prefix(&prefix_icon);

            let copy_btn = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(vec!["flat"])
                .tooltip_text(&crate::i18n::gettext("Copy address"))
                .build();
            let addr_for_copy = addr.clone();
            let page_weak = self.downgrade();
            copy_btn.connect_clicked(move |btn| {
                btn.display().clipboard().set_text(&addr_for_copy);
                if let Some(p) = page_weak.upgrade() {
                    p.show_toast(&crate::i18n::gettext("Address copied"));
                }
            });
            row.add_suffix(&copy_btn);

            let open_map_btn = gtk::Button::builder()
                .icon_name("web-browser-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(vec!["flat"])
                .tooltip_text(&crate::i18n::gettext("Open in Map"))
                .build();
            let addr_for_map = addr.clone();
            let target_clone = target.clone();
            open_map_btn.connect_clicked(move |_| {
                let (primary, fallback) = crate::services::settings_service::build_contact_map_uri(
                    &addr_for_map,
                    &target_clone,
                    prefer_local,
                );
                if gio::AppInfo::launch_default_for_uri(&primary, None::<&gio::AppLaunchContext>).is_err() {
                    if let Some(fb) = fallback {
                        let _ = gio::AppInfo::launch_default_for_uri(&fb, None::<&gio::AppLaunchContext>);
                    }
                }
            });
            row.add_suffix(&open_map_btn);
            addr_box.append(&row);
        }

        if show_maps {
            map_box.set_visible(true);
            #[cfg(target_os = "linux")]
            {
                if self.imp().map_webview.borrow().is_none() {
                    let view = webkit6::WebView::builder()
                        .hexpand(true)
                        .vexpand(false)
                        .height_request(220)
                        .css_classes(vec!["contact-map-webview"])
                        .build();
                    let frame = gtk::Frame::builder()
                        .css_classes(vec!["contact-map-frame"])
                        .child(&view)
                        .build();
                    map_box.append(&frame);
                    self.imp().map_webview.replace(Some(view));
                }
                if let Some(view) = self.imp().map_webview.borrow().as_ref() {
                    let primary_addr = &ordered_addrs[0];
                    let encoded_query = url_encode(primary_addr);
                    let embed_url = format!("https://www.openstreetmap.org/export/embed.html?layer=mapnik&q={encoded_query}");
                    view.load_uri(&embed_url);
                }
            }
        } else {
            map_box.set_visible(false);
        }
    }

    pub(crate) fn update_contact_structured_details(&self, record: Option<&EBookContactRecord>) {
        if let Some(cat_box) = self.imp().detail_categories_box.borrow().as_ref() {
            while let Some(child) = cat_box.first_child() {
                cat_box.remove(&child);
            }
            if let Some(rec) = record {
                let mut count = 0usize;
                for cat in rec.contact.categories.iter().filter(|c| !c.trim().is_empty()) {
                    let chip = gtk::Label::builder()
                        .label(cat.trim())
                        .css_classes(vec!["caption", "contact-category-chip"])
                        .build();
                    cat_box.append(&chip);
                    count += 1;
                }
                if !rec.source_display_name.trim().is_empty() {
                    let book_chip = gtk::Label::builder()
                        .label(rec.source_display_name.trim())
                        .css_classes(vec!["caption", "contact-row-badge"])
                        .build();
                    cat_box.append(&book_chip);
                    count += 1;
                }
                cat_box.set_visible(count > 0);
            } else {
                cat_box.set_visible(false);
            }
        }

        if let (Some(phone_group), Some(phone_box)) = (
            self.imp().phone_group.borrow().clone(),
            self.imp().phone_box.borrow().clone(),
        ) {
            while let Some(child) = phone_box.first_child() {
                phone_box.remove(&child);
            }
            if let Some(rec) = record {
                let c = &rec.contact;
                let mut phones: Vec<(String, String)> = Vec::new();
                for prop in &c.phone_properties {
                    let num = prop.value.trim();
                    if num.is_empty() || phones.iter().any(|(existing, _)| existing == num) {
                        continue;
                    }
                    let prop_upper = prop.property.to_ascii_uppercase();
                    let kind_label = if prop_upper.contains("CELL") || prop_upper.contains("MOBILE") {
                        crate::i18n::gettext("Mobile")
                    } else if prop_upper.contains("WORK") {
                        crate::i18n::gettext("Work")
                    } else if prop_upper.contains("HOME") {
                        crate::i18n::gettext("Home")
                    } else {
                        crate::i18n::gettext("Phone")
                    };
                    phones.push((num.to_string(), kind_label));
                }
                for num in &c.phones {
                    let trimmed = num.trim();
                    if !trimmed.is_empty() && !phones.iter().any(|(existing, _)| existing == trimmed) {
                        phones.push((trimmed.to_string(), crate::i18n::gettext("Phone")));
                    }
                }
                if !c.tel.trim().is_empty()
                    && !phones.iter().any(|(num, _)| num == c.tel.trim())
                {
                    phones.push((c.tel.trim().to_string(), crate::i18n::gettext("Primary")));
                }

                if phones.is_empty() {
                    phone_group.set_visible(false);
                } else {
                    phone_group.set_visible(true);
                    for (num, kind_label) in phones {
                        let row = adw::ActionRow::builder()
                            .title(&num)
                            .subtitle(&kind_label)
                            .selectable(false)
                            .css_classes(vec!["property"])
                            .build();
                        row.add_prefix(&gtk::Image::from_icon_name("call-start-symbolic"));

                        let copy_btn = gtk::Button::builder()
                            .icon_name("edit-copy-symbolic")
                            .valign(gtk::Align::Center)
                            .css_classes(vec!["flat"])
                            .tooltip_text(&crate::i18n::gettext("Copy"))
                            .build();
                        let num_copy = num.clone();
                        let page_weak = self.downgrade();
                        copy_btn.connect_clicked(move |btn| {
                            btn.display().clipboard().set_text(&num_copy);
                            if let Some(p) = page_weak.upgrade() {
                                p.show_toast(&crate::i18n::gettext("Copied to clipboard"));
                            }
                        });
                        row.add_suffix(&copy_btn);
                        phone_box.append(&row);
                    }
                }
            } else {
                phone_group.set_visible(false);
            }
        }

        if let (Some(details_group), Some(details_box)) = (
            self.imp().details_group.borrow().clone(),
            self.imp().details_box.borrow().clone(),
        ) {
            while let Some(child) = details_box.first_child() {
                details_box.remove(&child);
            }
            if let Some(rec) = record {
                let c = &rec.contact;
                let mut has_rows = false;
                let mut add_detail_row = |icon: &str, label: &str, value: &str| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        return;
                    }
                    has_rows = true;
                    let row = adw::ActionRow::builder()
                        .title(label)
                        .subtitle(trimmed)
                        .selectable(false)
                        .css_classes(vec!["property"])
                        .build();
                    row.add_prefix(&gtk::Image::from_icon_name(icon));

                    let copy_btn = gtk::Button::builder()
                        .icon_name("edit-copy-symbolic")
                        .valign(gtk::Align::Center)
                        .css_classes(vec!["flat"])
                        .tooltip_text(&crate::i18n::gettext("Copy"))
                        .build();
                    let val_copy = trimmed.to_string();
                    let page_weak = self.downgrade();
                    copy_btn.connect_clicked(move |btn| {
                        btn.display().clipboard().set_text(&val_copy);
                        if let Some(p) = page_weak.upgrade() {
                            p.show_toast(&crate::i18n::gettext("Copied to clipboard"));
                        }
                    });
                    row.add_suffix(&copy_btn);
                    details_box.append(&row);
                };

                let clean_org = clean_contact_org(&c.org);
                add_detail_row("x-office-address-book-symbolic", &crate::i18n::gettext("Organization"), &clean_org);
                add_detail_row("avatar-default-symbolic", &crate::i18n::gettext("Job Title"), &c.title);
                add_detail_row("avatar-default-symbolic", &crate::i18n::gettext("Role"), &c.role);
                add_detail_row("avatar-default-symbolic", &crate::i18n::gettext("Nickname"), &c.nickname);
                if !c.urls.is_empty() {
                    add_detail_row("web-browser-symbolic", &crate::i18n::gettext("Website"), &c.urls.join(", "));
                }
                add_detail_row("x-office-calendar-symbolic", &crate::i18n::gettext("Birthday"), &c.birthday);
                add_detail_row("x-office-calendar-symbolic", &crate::i18n::gettext("Anniversary"), &c.anniversary);
                if !c.impps.is_empty() {
                    add_detail_row("user-available-symbolic", &crate::i18n::gettext("Chat"), &c.impps.join(", "));
                }
                add_detail_row("document-edit-symbolic", &crate::i18n::gettext("Notes"), &c.note);

                if c.is_list && !c.list_members.is_empty() {
                    let members_str = c
                        .list_members
                        .iter()
                        .map(|m| m.display_text(c.list_show_addresses))
                        .collect::<Vec<_>>()
                        .join(", ");
                    add_detail_row(
                        "system-users-symbolic",
                        &crate::i18n::gettext("Members"),
                        &members_str,
                    );
                }

                details_group.set_visible(has_rows);
            } else {
                details_group.set_visible(false);
            }
        }
    }
}

pub(crate) fn contact_url_encode(input: &str) -> String {
    url_encode(input)
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{:02X}", b);
        }
    }
    out
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

    let has_existing = !stored_contacts.borrow().is_empty();
    if !has_existing {
        while let Some(row) = contact_list.row_at_index(0) {
            contact_list.remove(&row);
        }
        empty_state.set_label(&gettext("Loading contacts…"));
        empty_state.set_visible(true);
    }

    if let Some(page) = page_weak.upgrade() {
        if page.imp().disposed.get() {
            return;
        }
        if !has_existing {
            page.imp().selected_identity.replace(None);
            *selected_contact.borrow_mut() = None;
        }
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

    let (sender, receiver) = async_channel::bounded(2);
    std::thread::spawn(move || {
        // Fast path 1: Instant SQLite cache snapshot (<10ms)
        let fast_contacts = crate::services::evolution_contact_cache::load_contacts_from_evolution_sqlite_cache();
        if !fast_contacts.is_empty() {
            let fast_result = crate::services::ebook_service::EBookLoadResult {
                contacts: fast_contacts,
                origin: EBookDataOrigin::Local,
                warning: None,
            };
            let _ = sender.send_blocking((fast_result, true));
        }

        // Full path 2: EDS native / remote address book sync
        let result = load_contacts_prefer_eds();
        if let Err(error) = sender.send_blocking((result, false)) {
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
        while let Ok((result, from_cache)) = receiver.recv().await {
            if !is_current() || !page_is_alive() {
                return;
            }
            let warning = result.warning.as_deref();
            if let Some(warning) = warning {
                tracing::warn!(%warning, "Contact refresh completed with an EBook warning");
            }
            if result.contacts.is_empty() {
                if stored_contacts.borrow().is_empty() {
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
                }
                if !from_cache {
                    if let Some(page) = page_weak.upgrade() {
                        if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                            spinner.stop();
                            spinner.set_visible(false);
                        }
                    }
                    return;
                }
                continue;
            }

            let same_data = {
                let current = stored_contacts.borrow();
                current.len() == result.contacts.len()
                    && current.iter().zip(&result.contacts).all(|(a, b)| {
                        a.uid == b.uid
                            && a.contact.fn_name == b.contact.fn_name
                            && a.contact.email == b.contact.email
                    })
            };

            let contact_count = result.contacts.len();
            if let Some(page) = page_weak.upgrade() {
                page.update_drawer_total(contact_count);
                if !from_cache {
                    if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                        spinner.stop();
                        spinner.set_visible(false);
                    }
                }
            }
            empty_state.set_label(&contact_load_status_message(
                result.origin,
                contact_count,
                warning,
            ));
            empty_state.set_visible(contact_status_needs_visibility(contact_count, warning));

            if !same_data || contact_list.row_at_index(0).is_none() {
                while let Some(row) = contact_list.row_at_index(0) {
                    contact_list.remove(&row);
                }
                let contacts = result.contacts;
                let mut stored_ref = stored_contacts.borrow_mut();
                *stored_ref = contacts.clone();
                for record in &contacts {
                    append_contact_row(&contact_list, record);
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
                }
                start_contact_search(
                    page_weak.clone(),
                    search_state.active_query(),
                    Rc::clone(&search_state),
                    Rc::clone(&stored_contacts),
                );
            }

            if !from_cache {
                return;
            }
        }
        if let Some(page) = page_weak.upgrade() {
            if let Some(spinner) = page.imp().contact_search_spinner.borrow().as_ref() {
                spinner.stop();
                spinner.set_visible(false);
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
        gettext("Local Address Book")
    } else {
        gettext("Address Book")
    }
}

fn confirmed_contact_uid(backend_uid: &str, requested_uid: &str) -> String {
    if backend_uid.trim().is_empty() {
        requested_uid.to_string()
    } else {
        backend_uid.to_string()
    }
}

fn contact_origin_label(origin: EBookDataOrigin) -> String {
    match origin {
        EBookDataOrigin::Eds => gettext("synchronized address book"),
        EBookDataOrigin::Graph => gettext("external address book"),
        EBookDataOrigin::Local => gettext("local address book"),
    }
}

fn contact_load_status_message(
    origin: EBookDataOrigin,
    contact_count: usize,
    warning: Option<&str>,
) -> String {
    let message = if contact_count == 0 {
        match origin {
            EBookDataOrigin::Eds => gettext("No synchronized contacts loaded"),
            EBookDataOrigin::Graph => {
                gettext("No contacts loaded from external address book")
            }
            EBookDataOrigin::Local => {
                gettext("No contacts loaded from local address book")
            }
        }
    } else {
        crate::i18n::ngettext(
            "{n} contact loaded from {source}.",
            "{n} contacts loaded from {source}.",
            contact_count as u32,
        )
        .replace("{n}", &contact_count.to_string())
        .replace("{source}", &contact_origin_label(origin))
    };
    match warning.map(str::trim).filter(|warning| !warning.is_empty()) {
        Some(warning) => format!("{message} {}: {warning}", gettext("Note")),
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
    let fresh_uid = format!("meridian-{}", uuid::Uuid::new_v4());
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
        return Err(gettext("No contacts selected"));
    }
    let destination_uid = destination_uid.trim();
    if destination_uid.is_empty() || !destination_writable {
        return Err(gettext("The target address book is not writable"));
    }

    if selected.iter().all(|r| r.source_uid == destination_uid) {
        return Err(gettext("Source and destination are the same address book"));
    }

    let mut seen = HashSet::with_capacity(selected.len());
    let mut items = Vec::with_capacity(selected.len());

    for record in selected {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity.clone()) {
            return Err(gettext("A selected contact was resolved multiple times"));
        }
        if kind == ContactTransferKind::Move && !contact_source_is_writable(&record.source_uid) {
            return Err(
                gettext("At least one selected contact belongs to a read-only source"),
            );
        }
        let name = if record.contact.fn_name.trim().is_empty() {
            gettext("Unnamed Contact")
        } else {
            record.contact.fn_name.trim().to_string()
        };
        let source_display_name =
            contact_source_label(&record.source_display_name, &record.source_uid);
        let vcard = record.vcard.trim();
        if vcard.is_empty() {
            return Err(
                gettext("Contact “{name}” contains no valid vCard data for transfer")
                    .replace("{name}", &name),
            );
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
            gettext("Local Address Book"),
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
        page.show_toast(&crate::i18n::gettext("No other writable destination address book available"));
        return;
    }

    let dest_names: Vec<&str> = eligible.iter().map(|(_, name)| name.as_str()).collect();
    let dest_model = gtk::StringList::new(&dest_names);

    let count = selected_records.len();
    let (title, description, confirm_label, appearance) = match kind {
        ContactTransferKind::Copy => {
            let title = if count == 1 {
                crate::i18n::gettext("Copy Contact")
            } else {
                format!("{count} {}", crate::i18n::gettext("Copy Contacts"))
            };
            let description = if count == 1 {
                crate::i18n::gettext("Select the destination address book to copy the contact to:")
            } else {
                crate::i18n::gettext("Select the destination address book to copy the contacts to:")
            };
            (
                title,
                description,
                crate::i18n::gettext("Copy"),
                adw::ResponseAppearance::Suggested,
            )
        }
        ContactTransferKind::Move => {
            let title = if count == 1 {
                crate::i18n::gettext("Move Contact")
            } else {
                format!("{count} {}", crate::i18n::gettext("Move Contacts"))
            };
            let description = if count == 1 {
                crate::i18n::gettext("The contact will be created in the destination address book and then removed from the current address book.")
            } else {
                crate::i18n::gettext("The selected contacts will be created in the destination address book and then removed from their current address books.")
            };
            (
                title,
                description,
                crate::i18n::gettext("Move"),
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
    dialog.add_response("cancel", &crate::i18n::gettext("Cancel"));
    dialog.add_response("confirm", &confirm_label);
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
            page.show_toast(&crate::i18n::gettext("Invalid address book selection"));
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
                    gettext("The target address book is no longer writable"),
                ));
                break;
            }

            if plan.kind == ContactTransferKind::Move
                && !contact_source_is_writable(&item.source_uid)
            {
                result.failure = Some((
                    item.identity,
                    gettext("The source address book is no longer writable immediately before moving"),
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
                        gettext("Failed to create in target address book: {error}")
                            .replace("{error}", &error.to_string()),
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
                                gettext("The backend confirmed deletion of a different contact"),
                            ));
                            break;
                        }
                    }
                    Err(error) => {
                        result.failure = Some((
                            item.identity,
                            gettext("Failed to remove from source address book: {error}")
                                .replace("{error}", &error.to_string()),
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
                ctx.detail_name.set_label(&gettext("No contact selected"));
                ctx.detail_title
                    .set_label(&gettext("Address book data will be shown after synchronization"));
                ctx.detail_extra.set_label(&gettext("No further contact details"));
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
                gettext("{created_count} contact(s) transferred; error with {failed_id}: {error}")
                    .replace("{created_count}", &created_count.to_string())
                    .replace("{failed_id}", &failed_id.uid)
                    .replace("{error}", &error)
            } else {
                format!("{}: {error}", gettext("Transfer failed"))
            };
            show_toast_for_widget(&ctx.detail_name, &message);
        } else {
            let dest = &result.destination_display_name;
            let message = match result.kind {
                ContactTransferKind::Copy => {
                    crate::i18n::ngettext(
                        "1 contact copied to “{dest}”",
                        "{n} contacts copied to “{dest}”",
                        created_count as u32,
                    )
                    .replace("{n}", &created_count.to_string())
                    .replace("{dest}", dest)
                }
                ContactTransferKind::Move => {
                    crate::i18n::ngettext(
                        "1 contact moved to “{dest}”",
                        "{n} contacts moved to “{dest}”",
                        created_count as u32,
                    )
                    .replace("{n}", &created_count.to_string())
                    .replace("{dest}", dest)
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
            Self::NoSelection => gettext("No contacts selected"),
            Self::InvalidVCard { contact_name } => {
                gettext("vCard export not started: “{contact_name}” contains no complete vCard")
                    .replace("{contact_name}", contact_name)
            }
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
        return Err(gettext("No contacts selected"));
    }

    let mut seen = HashSet::with_capacity(selected.len());
    let mut items = Vec::with_capacity(selected.len());

    for record in selected {
        let identity = ContactIdentity::new(record.source_uid.clone(), record.uid.clone());
        if !seen.insert(identity.clone()) {
            return Err(gettext("A selected contact was resolved multiple times"));
        }
        if !contact_source_is_writable(&record.source_uid) {
            return Err(
                gettext("At least one selected contact belongs to a read-only source"),
            );
        }
        let name = if record.contact.fn_name.trim().is_empty() {
            gettext("Unnamed Contact")
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
            title: gettext("Delete contact?"),
            description: gettext(
                "The contact “{name}” will be permanently removed from the address book “{source}”.",
            )
            .replace("{name}", &item.name)
            .replace("{source}", &item.source_display_name),
        }
    } else {
        ContactDeleteConfirmation {
            title: gettext("Delete contacts?"),
            description: crate::i18n::ngettext(
                "Are you sure you want to permanently remove {n} selected contact from the address books?",
                "Are you sure you want to permanently remove {n} selected contacts from the address books?",
                items.len() as u32,
            )
            .replace("{n}", &items.len().to_string()),
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
    let email_tt = if state.email {
        gettext("Send email to selected contacts")
    } else {
        gettext("Selection contains no email address")
    };
    email_button.set_tooltip_text(Some(&email_tt));

    let export_tt = if state.export {
        gettext("Export selected contacts together as vCard")
    } else if state.selection_count == 0 {
        gettext("No contact selected")
    } else {
        gettext("Selection contains no fully exportable vCard")
    };
    export_button.set_tooltip_text(Some(&export_tt));

    let print_tt = if state.selection_count == 0 {
        gettext("No contact selected")
    } else if multiple {
        gettext("Print selected contacts")
    } else {
        gettext("Print selected contact")
    };
    print_button.set_tooltip_text(Some(&print_tt));

    let copy_tt = if state.selection_count == 0 {
        gettext("No contact selected")
    } else if multiple {
        gettext("Copy selected contacts to another address book")
    } else {
        gettext("Copy contact to another address book")
    };
    transfer_copy_button.set_tooltip_text(Some(&copy_tt));

    let move_tt = if state.selection_count == 0 {
        gettext("No contact selected")
    } else if !state.transfer_move {
        gettext("Selection contains contacts from a read-only source")
    } else if multiple {
        gettext("Move selected contacts to another address book")
    } else {
        gettext("Move contact to another address book")
    };
    transfer_move_button.set_tooltip_text(Some(&move_tt));

    let edit_tt = if multiple {
        gettext("Edit is only available for a single contact")
    } else {
        gettext("Edit contact")
    };
    edit_button.set_tooltip_text(Some(&edit_tt));

    let merge_tt = if multiple {
        gettext("Merge is only available for a single contact")
    } else {
        gettext("Merge duplicates from the same source")
    };
    merge_button.set_tooltip_text(Some(&merge_tt));

    let delete_tt = if state.selection_count == 0 {
        gettext("No contact selected")
    } else if !state.delete {
        gettext("Selection contains contacts from a read-only source")
    } else if multiple {
        gettext("Delete selected contacts")
    } else {
        gettext("Delete contact")
    };
    delete_button.set_tooltip_text(Some(&delete_tt));
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
        return Err(gettext("No contacts selected"));
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
                    gettext("Contact selection is no longer current; please reselect"),
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

    let active_query = search_state.active_query();
    let has_active_filter = !active_query.clauses.is_empty()
        || active_query.filters.source_uid.is_some()
        || active_query.filters.category.is_some()
        || active_query.filters.unmatched_category
        || active_query.filters.duplicate_only;

    let mut child = contact_list.first_child();
    while let Some(current) = child {
        let next = current.next_sibling();
        if let Ok(row) = current.downcast::<gtk::ListBoxRow>() {
            if let Some(identity) = row_contact_identity(&row) {
                let is_visible = if !has_active_filter {
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
                                gettext("A visible contact is no longer uniquely available"),
                            );
                        }
                    }
                }
            }
        }
        child = next;
    }

    if visible.is_empty() {
        return Err(gettext("No visible contacts available to print"));
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
            "contact.vcf".to_string()
        } else {
            "contacts.vcf".to_string()
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
        return Err(gettext("No contacts selected"));
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
                    "contact".to_string()
                } else {
                    name.chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                        .collect::<String>()
                };
                let stem = if safe_stem.is_empty() {
                    "contact"
                } else {
                    &safe_stem
                };
                format!("{stem}.{}", format.extension())
            } else {
                format!("contacts.{}", format.extension())
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
            Ok(_) => {
                let msg = crate::i18n::ngettext(
                    "Contact exported as {format}",
                    "{n} contacts exported as {format}",
                    contact_count as u32,
                )
                .replace("{n}", &contact_count.to_string())
                .replace("{format}", &format.display_label());
                show_toast_for_widget(&button, &msg);
            }
            Err((_, error)) => show_toast_for_widget(
                &button,
                &gettext("Export could not be saved: {error}")
                    .replace("{error}", &error.to_string()),
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
    detail_name.set_label(&crate::i18n::gettext("No contact selected"));
    detail_title.set_label(&crate::i18n::gettext("Address book data will be displayed after synchronization"));
    detail_extra.set_label(&crate::i18n::gettext("No further contact details"));
    email_row.set_title(&crate::i18n::gettext("No email address"));
    email_row.set_subtitle(&crate::i18n::gettext("No contact selected"));
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
        Err(error) if error == gettext("No contacts selected") => Vec::new(),
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
            detail_extra.set_visible(false);
            page.update_contact_address_and_map(None);
            page.update_contact_structured_details(None);
            if let Some(pill) = page.imp().contact_actions_pill.borrow().as_ref() {
                pill.set_visible(false);
            }
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
            reset_contact_detail(
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            detail_extra.set_visible(false);
            page.update_contact_address_and_map(None);
            page.update_contact_structured_details(None);
        }
        [record] => {
            *selected_contact.borrow_mut() = Some(record.clone());
            page.imp()
                .selected_identity
                .replace(Some(ContactIdentity::new(
                    record.source_uid.clone(),
                    record.uid.clone(),
                )));
            update_contact_detail(
                record,
                detail_avatar,
                detail_name,
                detail_title,
                detail_extra,
                email_row,
            );
            detail_extra.set_visible(false);
            page.update_contact_address_and_map(Some(&record.contact));
            page.update_contact_structured_details(Some(record));
        }
        records => {
            *selected_contact.borrow_mut() = None;
            page.imp().selected_identity.replace(None);
            detail_avatar.set_text(Some(""));
            detail_avatar.set_custom_image(None::<&gdk::Texture>);
            let count = records.len();
            let count_label = if count == 1 {
                crate::i18n::gettext("1 contact selected")
            } else {
                format!("{count} {}", crate::i18n::gettext("contacts selected"))
            };
            detail_name.set_label(&count_label);
            detail_title.set_label(&crate::i18n::gettext(
                "Email, Print, Delete, and vCard Export use the full selection; Edit and Merge require exactly one contact.",
            ));
            let email_subtitle = if state.email {
                crate::i18n::gettext("Email addresses can be used together")
            } else {
                crate::i18n::gettext("No email addresses in selection")
            };
            email_row.set_subtitle(&email_subtitle);
            let extra_label = if state.delete {
                crate::i18n::gettext("Multiple selection: Selected contacts can be printed, exported, or deleted together.")
            } else {
                crate::i18n::gettext("Multiple selection: Print and Export write verified vCards. Deletion is disabled for read-only sources.")
            };
            detail_extra.set_label(&extra_label);
            detail_extra.set_visible(true);
            page.update_contact_address_and_map(None);
            page.update_contact_structured_details(None);
        }
    }
    let selected_ids = current_contact_selected_identities(contact_list);
    if let Some(layout) = page.imp().contact_table_layout.borrow_mut().as_mut() {
        for row in layout.rows.iter_mut() {
            row.selected = selected_ids.contains(&row.identity);
        }
    }
    page.queue_contacts_table_draw();
    if let Some(pill) = page.imp().contact_actions_pill.borrow().as_ref() {
        pill.set_visible(!selected.is_empty());
    }
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
    page.imp().selected_identity.replace(None);
    if let Some(pill) = page.imp().contact_actions_pill.borrow().as_ref() {
        pill.set_visible(false);
    }
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

            let current_view = page
                .imp()
                .view_stack
                .borrow()
                .as_ref()
                .and_then(|stack| stack.visible_child_name())
                .unwrap_or_else(|| glib::GString::from("simple"));

            if current_view == "table" {
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
            } else {
                // "simple" list view
                if target_row_clone.is_mapped() {
                    target_row_clone.grab_focus();
                    if let Some(scroll) = page.imp().simple_list_scroll.borrow().as_ref() {
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
            status.set_text(&gettext("Search not applied: {error}").replace("{error}", &error.to_string()));
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
        status.set_text(&gettext("Searching contacts…"));
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
                    status.set_text(&gettext("Search not applied: {error}").replace("{error}", &error.to_string()));
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
                status.set_text(&gettext("No contacts match the search"));
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
    let no_email_str = gettext("No email address");
    let no_phone_str = gettext("No phone number");
    email_row.set_title(if email_text.is_empty() {
        &no_email_str
    } else {
        email_text.as_str()
    });
    email_row.set_subtitle(if phone_text.is_empty() {
        &no_phone_str
    } else {
        phone_text.as_str()
    });
    let detail_title_text = contact_detail_title(&record.contact);
    detail_title.set_label(&detail_title_text);
    detail_extra.set_label(&contact_detail_summary(&record.contact));
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
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
    row.add_css_class("contact-list-row");
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
        .margin_start(6)
        .margin_end(6)
        .margin_top(4)
        .margin_bottom(4)
        .build();

    let clean_name = crate::views::clean_name_for_avatar(&contact.fn_name);
    let avatar_name = if clean_name.is_empty() {
        &contact.fn_name
    } else {
        &clean_name
    };
    let avatar = adw::Avatar::builder()
        .size(40)
        .text(avatar_name)
        .show_initials(true)
        .valign(gtk::Align::Center)
        .build();
    set_avatar_photo(&avatar, contact.photo.as_ref());
    row_box.append(&avatar);

    let info_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();

    // Line 1: Name + Category / List badge
    let line1_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
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
    line1_box.append(&name);

    let badge_text = if contact.is_list {
        Some(crate::i18n::gettext("List"))
    } else if let Some(cat) = contact.categories.first().filter(|c| !c.trim().is_empty()) {
        Some(cat.trim().to_string())
    } else {
        None
    };
    if let Some(text) = badge_text {
        let badge = gtk::Label::builder()
            .label(&text)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .css_classes(vec!["caption", "contact-row-badge"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        line1_box.append(&badge);
    }
    info_box.append(&line1_box);

    // Line 2: Email (or phone)
    let email_text = contact_email_text(contact);
    let line2_text = if !email_text.is_empty() {
        email_text.clone()
    } else if !contact.tel.is_empty() {
        contact.tel.clone()
    } else {
        String::new()
    };
    if !line2_text.is_empty() {
        let line2_label = gtk::Label::builder()
            .label(&line2_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .lines(1)
            .width_chars(1)
            .build();
        info_box.append(&line2_label);
    }

    // Line 3: Organization • Title (dim caption)
    let clean_org = clean_contact_org(&contact.org);
    let mut meta_parts = Vec::new();
    if !clean_org.is_empty() {
        meta_parts.push(clean_org.as_str());
    }
    if !contact.title.trim().is_empty() {
        meta_parts.push(contact.title.trim());
    }
    if !email_text.is_empty() && !contact.tel.trim().is_empty() && meta_parts.is_empty() {
        meta_parts.push(contact.tel.trim());
    }
    let meta_text = meta_parts.join(" • ");
    if !meta_text.is_empty() {
        let meta_label = gtk::Label::builder()
            .label(&meta_text)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["dim-label", "caption"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .lines(1)
            .width_chars(1)
            .build();
        info_box.append(&meta_label);
    }

    row_box.append(&info_box);
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
            (gettext("Mobile"), "call-start-symbolic")
        } else if prop_upper.contains("WORK") || prop_upper.contains("BUSINESS") {
            (gettext("Business"), "call-start-symbolic")
        } else if prop_upper.contains("HOME") {
            (gettext("Home"), "call-start-symbolic")
        } else if prop_upper.contains("FAX") {
            (gettext("Fax"), "printer-symbolic")
        } else {
            (gettext("Phone"), "call-start-symbolic")
        };
        result.push(ContactCardPhone {
            label,
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
                (String::new(), "call-start-symbolic")
            } else {
                match idx + result.len() {
                    0 => (gettext("Mobile"), "call-start-symbolic"),
                    1 => (gettext("Business"), "call-start-symbolic"),
                    2 => (gettext("Home"), "call-start-symbolic"),
                    _ => (gettext("Phone"), "call-start-symbolic"),
                }
            };
            result.push(ContactCardPhone {
                label: lbl,
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
        return crate::i18n::ngettext(
            "Contact list · {n} member · addresses hidden",
            "Contact list · {n} members · addresses hidden",
            contact.list_members.len() as u32,
        )
        .replace("{n}", &contact.list_members.len().to_string());
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
        clean_contact_org(&contact.org)
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
        lines.push(
            crate::i18n::ngettext(
                "Contact list: {n} member",
                "Contact list: {n} members",
                contact.list_members.len() as u32,
            )
            .replace("{n}", &contact.list_members.len().to_string()),
        );
        if contact.list_show_addresses {
            let members = contact
                .list_members
                .iter()
                .map(|member| member.display_text(true))
                .collect::<Vec<_>>();
            if !members.is_empty() {
                lines.push(
                    gettext("Members: {members}").replace("{members}", &members.join(" · ")),
                );
            }
        } else {
            lines.push(gettext("Member addresses: hidden"));
        }
    }
    let ordered_addrs = ordered_contact_addresses(contact, home_before_work);
    if !ordered_addrs.is_empty() {
        let separator = if address_formatting {
            "\n         "
        } else {
            " · "
        };
        lines.push(gettext("Address: {address}").replace("{address}", &ordered_addrs.join(separator)));
        if show_maps {
            let service_name = if map_target.eq_ignore_ascii_case("google") {
                "Google Maps"
            } else {
                "OpenStreetMap"
            };
            lines.push(gettext("Map: active ({service})").replace("{service}", service_name));
        }
    }
    if !contact.categories.is_empty() {
        lines.push(gettext("Categories: {categories}").replace("{categories}", &contact.categories.join(", ")));
    }
    if !contact.urls.is_empty() {
        lines.push(gettext("Websites: {websites}").replace("{websites}", &contact.urls.join(", ")));
    }
    if !contact.nickname.trim().is_empty() {
        lines.push(gettext("Nickname: {nickname}").replace("{nickname}", &contact.nickname));
    }
    if !contact.birthday.trim().is_empty() {
        lines.push(gettext("Birthday: {birthday}").replace("{birthday}", &contact.birthday));
    }
    if !contact.anniversary.trim().is_empty() {
        lines.push(gettext("Anniversary: {anniversary}").replace("{anniversary}", &contact.anniversary));
    }
    if !contact.role.trim().is_empty() {
        lines.push(gettext("Role: {role}").replace("{role}", &contact.role));
    }
    if !contact.gender.trim().is_empty() {
        lines.push(gettext("Gender: {gender}").replace("{gender}", &contact.gender));
    }
    if !contact.timezone.trim().is_empty() {
        lines.push(gettext("Timezone: {timezone}").replace("{timezone}", &contact.timezone));
    }
    if !contact.geo.trim().is_empty() {
        lines.push(gettext("Position: {position}").replace("{position}", &contact.geo));
    }
    if !contact.impps.is_empty() {
        lines.push(gettext("Chat/IMPP: {impp}").replace("{impp}", &contact.impps.join(", ")));
    }
    if !contact.related.is_empty() {
        lines.push(gettext("Related: {related}").replace("{related}", &contact.related.join(", ")));
    }
    if !contact.note.trim().is_empty() {
        lines.push(gettext("Note: {note}").replace("{note}", &contact.note));
    }
    if lines.is_empty() {
        gettext("No further contact details")
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
        ContactsPage, ContactViewMode, append_contact_row,
        ContactActionState, ContactIdentity, ContactPageSearchState, ContactSelectionRestore,
        ContactTransferKind, ContactVCardExportError, confirmed_contact_uid, contact_action_state,
        contact_category_matches, contact_detail_summary, contact_detail_summary_with_options,
        contact_email_text, contact_load_status_message, contact_query_matches,
        contact_recipient_text, contact_recipient_text_with_records,
        contact_records_are_duplicates, contact_records_share_duplicate_key,
        contact_selection_action_state, contact_sort_key, contact_source_label,
        contact_origin_label,
        contact_status_needs_visibility, context_action_state, ordered_contact_addresses,
        plan_contact_delete_batch, plan_contact_export, plan_contact_transfer,
        plan_contact_vcard_export, prepare_vcard_for_transfer, quick_field_for_index, quick_query,
        reduce_contact_delete_result, reduce_contact_transfer_result,
        should_restore_contact_selection, with_contact_outer_filters,
        contact_card_phones,
    };
    use crate::services::contact_export_service::ContactExportFormat;
    use crate::services::contact_search_service::{
        ContactClauseGroup, ContactOuterFilters, ContactSearchClause, ContactSearchField,
        ContactSearchOperator, ContactSearchQuery,
    };
    use crate::services::ebook_service::{EBookContactRecord, EBookDataOrigin};
    use crate::services::vcard_service::VCardContact;
    use gtk::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use super::{
        hit_test_contact_table, hit_test_contact_table_header, prepare_contact_table_layout,
        prepare_contact_table_layout_with_columns, ContactTableColumn,
    };

    fn record(source_uid: &str, uid: &str, contact: VCardContact) -> EBookContactRecord {
        EBookContactRecord {
            source_uid: source_uid.to_string(),
            source_display_name: "Test Address Book".to_string(),
            uid: uid.to_string(),
            vcard: contact.to_vcard_string(),
            contact,
        }
    }

    #[test]
    fn contact_category_filter_is_case_insensitive() {
        assert!(contact_category_matches("Work\u{1f}Family", "work"));
        assert!(contact_category_matches("Work\u{1f}Family", "FAMILY"));
        assert!(!contact_category_matches("Work\u{1f}Family", "Personal"));
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

        assert_eq!(plan.initial_name, "contacts.vcf");
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
        assert_eq!(csv_plan.initial_name, "contacts.csv");
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
            contact_source_label("Work Book", "goa-secret-source"),
            "Work Book"
        );
        assert_eq!(
            contact_source_label("", "local-addressbook"),
            crate::i18n::gettext("Local Address Book")
        );
        assert_eq!(
            contact_source_label("", "uid-only-source"),
            crate::i18n::gettext("Address Book")
        );
    }

    #[test]
    fn load_status_keeps_fallback_and_warning_honest() {
        assert_eq!(
            contact_load_status_message(EBookDataOrigin::Eds, 4, None),
            crate::i18n::ngettext(
                "{n} contact loaded from {source}.",
                "{n} contacts loaded from {source}.",
                4,
            )
            .replace("{n}", "4")
            .replace("{source}", &contact_origin_label(EBookDataOrigin::Eds))
        );
        let fallback = contact_load_status_message(
            EBookDataOrigin::Local,
            2,
            Some("The address book service is unavailable"),
        );
        assert!(fallback.contains(&contact_origin_label(EBookDataOrigin::Local)));
        assert!(fallback.contains(&crate::i18n::gettext("Note")));
        assert!(contact_status_needs_visibility(0, None));
        assert!(!contact_status_needs_visibility(2, None));
        assert!(contact_status_needs_visibility(2, Some("Warning")));
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
        assert!(contact_email_text(&list).contains(&crate::i18n::gettext("addresses hidden")));
        assert!(!contact_email_text(&list).contains("external@example.test"));
        assert_eq!(
            contact_recipient_text(&list),
            "external@example.test, ada@example.test"
        );
        assert!(contact_detail_summary(&list).contains(&crate::i18n::gettext("Member addresses: hidden")));
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
        assert!(contact_email_text(&outer).contains(&crate::i18n::gettext("addresses hidden")));
        assert!(!contact_email_text(&outer).contains("nested@example.test"));
    }

    #[test]
    fn plan_contact_delete_batch_rejects_empty_selection() {
        let result = plan_contact_delete_batch(&[]);
        assert_eq!(result, Err(crate::i18n::gettext("No contacts selected")));
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
        assert_eq!(plan.confirmation.title, crate::i18n::gettext("Delete contact?"));
        assert!(plan.confirmation.description.contains("Ada Lovelace"));
        assert!(plan.confirmation.description.contains("Test Address Book"));
    }

    #[test]
    fn plan_contact_delete_batch_multiple_contacts_creates_count_confirmation() {
        let c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "", "");
        let c2 = VCardContact::new("Grace Hopper", "grace@example.test", "", "");
        let r1 = record("book-a", "ada-uid", c1);
        let r2 = record("book-a", "grace-uid", c2);
        let plan = plan_contact_delete_batch(&[r1, r2]).expect("plan succeeds");
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.confirmation.title, crate::i18n::gettext("Delete contacts?"));
        assert!(
            plan.confirmation
                .description
                .contains("2")
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
            Err(crate::i18n::gettext("At least one selected contact belongs to a read-only source"))
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
            Err(crate::i18n::gettext("A selected contact was resolved multiple times"))
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
        assert!(transformed.contains("UID:meridian-"));
        assert!(transformed.contains("FN:Ada Lovelace"));
    }

    #[test]
    fn prepare_vcard_for_transfer_inserts_uid_if_missing() {
        let vcard = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEND:VCARD";
        let transformed = prepare_vcard_for_transfer(vcard);
        assert!(transformed.contains("UID:meridian-"));
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
        assert_eq!(result, Err(crate::i18n::gettext("No contacts selected")));
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
            Err(crate::i18n::gettext("The target address book is not writable"))
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
            Err(crate::i18n::gettext("Source and destination are the same address book"))
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
            Err(crate::i18n::gettext("At least one selected contact belongs to a read-only source"))
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
            Err(crate::i18n::gettext("A selected contact was resolved multiple times"))
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
        assert!(summary_osm_map.contains(&crate::i18n::gettext("Address: {address}").replace("{address}", "Hauptstraße 1, 10115 Berlin")));
        assert!(summary_osm_map.contains(&crate::i18n::gettext("Map: active ({service})").replace("{service}", "OpenStreetMap")));

        let summary_google_map =
            contact_detail_summary_with_options(&contact, false, true, "google", false);
        assert!(summary_google_map.contains(&crate::i18n::gettext("Map: active ({service})").replace("{service}", "Google Maps")));

        let summary_no_map =
            contact_detail_summary_with_options(&contact, false, false, "openstreetmap", false);
        assert!(!summary_no_map.contains(&crate::i18n::gettext("Map: active ({service})").replace("{service}", "OpenStreetMap")));

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
        assert!(formatted.contains(&crate::i18n::gettext("Address: {address}").replace("{address}", "Straße 1, Berlin\n         Weg 2, Hamburg")));
    }

    #[test]
    fn test_contacts_paned_divider() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let page = super::ContactsPage::new();
        let paned = page.outer_paned().expect("outer paned exists");

        if let Some(settings) = crate::services::settings_service::AppSettings::try_new() {
            settings.set_mail_sidebar_position(260);
            settings.set_contact_hpane_position(260);

            // Re-sync with position
            paned.set_position(340);
            while context.iteration(false) {}
            assert_eq!(settings.contact_hpane_position(), 340);
            assert_eq!(settings.mail_sidebar_position(), 340);

            // Moving paned writes unified mail_sidebar_position and contact_hpane_position
            paned.set_position(440);
            while context.iteration(false) {}
            assert_eq!(settings.contact_hpane_position(), 440);
            assert_eq!(settings.mail_sidebar_position(), 440);

            // External change to mail_sidebar_position updates paned
            settings.set_mail_sidebar_position(280);
            while context.iteration(false) {}
            assert_eq!(paned.position(), 280);
        } else {
            assert!(paned.position() >= crate::window::MIN_SIDEBAR_WIDTH && paned.position() <= crate::window::MAX_SIDEBAR_WIDTH);
        }
    }

    #[test]
    fn test_adversarial_contacts_paned_divider_stress() {
        let _lock = crate::window::GTK_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else { return; };

        let page = super::ContactsPage::new();
        page.set_sidebar_visible(true);
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

        // Paned dragging out-of-bounds: < MIN_SIDEBAR_WIDTH (e.g. 50) clamps to MIN_SIDEBAR_WIDTH
        paned.set_position(50);
        while context.iteration(false) {}
        assert_eq!(paned.position(), crate::window::MIN_SIDEBAR_WIDTH);
        assert_eq!(settings.mail_sidebar_position(), crate::window::MIN_SIDEBAR_WIDTH);

        // Paned dragging out-of-bounds: > MAX_SIDEBAR_WIDTH (e.g. 2000) clamps to MAX_SIDEBAR_WIDTH
        paned.set_position(2000);
        while context.iteration(false) {}
        assert_eq!(paned.position(), crate::window::MAX_SIDEBAR_WIDTH);
        assert_eq!(settings.mail_sidebar_position(), crate::window::MAX_SIDEBAR_WIDTH);

        // Valid boundaries: MIN_SIDEBAR_WIDTH and MAX_SIDEBAR_WIDTH
        paned.set_position(crate::window::MIN_SIDEBAR_WIDTH);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), crate::window::MIN_SIDEBAR_WIDTH);

        paned.set_position(crate::window::MAX_SIDEBAR_WIDTH);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), crate::window::MAX_SIDEBAR_WIDTH);

        // External setting change with large value clamps UI paned to MAX_SIDEBAR_WIDTH
        settings.set_mail_sidebar_position(2500);
        while context.iteration(false) {}
        assert_eq!(
            paned.position(),
            crate::window::MAX_SIDEBAR_WIDTH,
            "UI paned must clamp large external GSettings to MAX_SIDEBAR_WIDTH"
        );

        // External setting change below min clamps UI paned to MIN_SIDEBAR_WIDTH
        settings.set_mail_sidebar_position(40);
        while context.iteration(false) {}
        assert_eq!(
            paned.position(),
            crate::window::MIN_SIDEBAR_WIDTH,
            "UI paned must clamp small external GSettings to MIN_SIDEBAR_WIDTH"
        );

        // 2. Window View Switching & Unified Sidebar Width Across Views
        let app = crate::application::MeridianApplication::new();
        let window = crate::window::MeridianWindow::new(&app);

        settings.set_mail_sidebar_position(260);
        while context.iteration(false) {}

        // Switching between views keeps unified sidebar widths without layout jumps
        window.set_active_view("mail");
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 260);

        window.set_active_view("contacts");
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 260);

        // Altering sidebar while in contacts updates unified mail_sidebar_position
        settings.set_mail_sidebar_position(350);
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 350);

        // Switching back to mail view preserves unified mail_sidebar_position
        window.set_active_view("mail");
        while context.iteration(false) {}
        assert_eq!(settings.mail_sidebar_position(), 350);
    }

    #[test]
    fn test_contact_view_mode_toggle_and_settings() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();

        // Switch to Table
        page.set_view_mode(ContactViewMode::Table);
        assert_eq!(page.current_view_mode(), ContactViewMode::Table);
        assert_eq!(
            page.imp().view_stack.borrow().as_ref().and_then(|s| s.visible_child_name()).as_deref(),
            Some("table")
        );

        // Switch back to Simple
        page.set_view_mode(ContactViewMode::Simple);
        assert_eq!(page.current_view_mode(), ContactViewMode::Simple);
        assert_eq!(
            page.imp().view_stack.borrow().as_ref().and_then(|s| s.visible_child_name()).as_deref(),
            Some("simple")
        );
    }

    #[test]
    fn test_contact_compact_row_3_line_hierarchy() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let list = gtk::ListBox::new();
        let mut contact = VCardContact::new("Grace Hopper", "grace@navy.mil", "+1 703 1234", "US Navy");
        contact.title = "Rear Admiral".to_string();
        contact.categories = vec!["Computer Science".to_string()];
        let rec = record("source-1", "uid-grace", contact);

        append_contact_row(&list, &rec);
        let row = list.row_at_index(0).expect("row at 0 exists");
        let row_box = row.child().and_then(|w| w.downcast::<gtk::Box>().ok()).expect("row_box exists");

        // Verify avatar (first child)
        let avatar = row_box.first_child().and_then(|w| w.downcast::<adw::Avatar>().ok()).expect("avatar exists");
        assert_eq!(avatar.size(), 40);

        // Verify info_box (second child)
        let info_box = row_box.last_child().and_then(|w| w.downcast::<gtk::Box>().ok()).expect("info_box exists");
        assert_eq!(info_box.orientation(), gtk::Orientation::Vertical);

        // Line 1: box containing Name + Badge
        let line1 = info_box.first_child().and_then(|w| w.downcast::<gtk::Box>().ok()).expect("line1 exists");
        let name_lbl = line1.first_child().and_then(|w| w.downcast::<gtk::Label>().ok()).expect("name label exists");
        assert_eq!(name_lbl.label().as_str(), "Grace Hopper");
        assert!(name_lbl.has_css_class("heading"));

        let badge_lbl = line1.last_child().and_then(|w| w.downcast::<gtk::Label>().ok()).expect("badge label exists");
        assert_eq!(badge_lbl.label().as_str(), "Computer Science");
        assert!(badge_lbl.has_css_class("contact-row-badge"));
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
        assert_eq!(mobile_phones[0].label, crate::i18n::gettext("Mobile"));
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
        assert_eq!(multi_phones[0].label, crate::i18n::gettext("Business"));
        assert_eq!(multi_phones[0].number, "+49 89 10001");
        assert_eq!(multi_phones[0].icon, "call-start-symbolic");
        assert_eq!(multi_phones[1].label, crate::i18n::gettext("Home"));
        assert_eq!(multi_phones[1].number, "+49 89 20002");
        assert_eq!(multi_phones[1].icon, "call-start-symbolic");
        assert_eq!(multi_phones[2].label, crate::i18n::gettext("Mobile"));
        assert_eq!(multi_phones[2].number, "+49 170 30003");
        assert_eq!(multi_phones[2].icon, "call-start-symbolic");
    }

    #[test]
    fn test_contact_table_drilldown_and_back_navigation() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();
        page.set_view_mode(ContactViewMode::Table);

        // Initially not drilled down
        assert!(!page.can_navigate_back());
        assert!(!page.imp().is_detail_shown.get());

        // When a row is activated / clicked in Table view, drill down occurs
        page.imp().is_detail_shown.set(true);
        page.sync_contacts_layout_visibility();
        assert!(page.can_navigate_back());

        // Back navigation returns to full-width table
        assert!(page.navigate_back());
        assert!(!page.can_navigate_back());
        assert!(!page.imp().is_detail_shown.get());

        // Subsequent navigate_back returns false
        assert!(!page.navigate_back());
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
    fn test_contacts_stale_default_address_book_does_not_create_unavailable_source() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        if let Some(settings) = crate::services::settings_service::AppSettings::try_new() {
            let original_default = settings.default_address_book_source_uid();
            settings.set_default_address_book_source_uid("account-contacts-dead-beef-source-id");

            let page = ContactsPage::new();
            page.reload_address_books();
            for _ in 0..10 {
                while context.iteration(false) {}
            }

            // Restore original setting
            settings.set_default_address_book_source_uid(&original_default);
        }
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
    fn test_contact_table_custom_columns_and_reordering() {
        use crate::services::settings_service::normalize_contact_table_column_order;

        // 1. Check ContactTableColumn properties and round-trips
        for id in &crate::services::settings_service::CONTACT_TABLE_COLUMN_IDS {
            let col = ContactTableColumn::from_id(id).expect("valid column id");
            assert_eq!(col.id(), *id);
            assert!(!col.label().is_empty());
            assert!(col.default_width() >= col.min_width());
        }

        // 2. Column order normalization
        let normalized = normalize_contact_table_column_order("email,phone,name");
        assert_eq!(normalized[0], "email");
        assert_eq!(normalized[1], "phone");
        assert_eq!(normalized[2], "name");
        assert_eq!(normalized.len(), 6); // Remaining columns appended

        // Invalid columns filtered out, duplicates removed
        let norm_weird = normalize_contact_table_column_order("phone,unknown_col,phone");
        assert_eq!(norm_weird[0], "phone");
        assert_eq!(norm_weird.len(), 6);

        // 3. Layout with custom column order & widths
        let mut c1 = VCardContact::new("Ada Lovelace", "ada@example.test", "12345", "Countess");
        c1.org = "Babbage Engines".to_string();
        let r1 = record("book-1", "id-ada", c1);
        let contacts = vec![r1];
        let selected = std::collections::HashSet::new();

        let custom_cols = vec![
            (ContactTableColumn::Email, 250.0),
            (ContactTableColumn::Name, 150.0),
            (ContactTableColumn::Phone, 100.0),
        ];

        let layout = prepare_contact_table_layout_with_columns(
            &contacts,
            None,
            &selected,
            ContactTableColumn::Name,
            false,
            800.0,
            &custom_cols,
        );

        assert_eq!(layout.columns.len(), 3);
        assert_eq!(layout.columns[0].0, ContactTableColumn::Email);
        assert_eq!(layout.columns[0].1, 250.0);
        assert_eq!(layout.columns[1].0, ContactTableColumn::Name);
        assert_eq!(layout.columns[1].1, 150.0);
        assert_eq!(layout.columns[2].0, ContactTableColumn::Phone);
        assert_eq!(layout.columns[2].1, 100.0);

        // Hit testing matches custom order:
        // x in [0..250] is Email
        assert_eq!(hit_test_contact_table_header(&layout, 10.0), Some(ContactTableColumn::Email));
        assert_eq!(hit_test_contact_table_header(&layout, 240.0), Some(ContactTableColumn::Email));
        // x in [250..400] is Name
        assert_eq!(hit_test_contact_table_header(&layout, 260.0), Some(ContactTableColumn::Name));
        assert_eq!(hit_test_contact_table_header(&layout, 390.0), Some(ContactTableColumn::Name));
        // x in [400..500] is Phone
        assert_eq!(hit_test_contact_table_header(&layout, 410.0), Some(ContactTableColumn::Phone));
        assert_eq!(hit_test_contact_table_header(&layout, 490.0), Some(ContactTableColumn::Phone));
        // x > 500 is None
        assert_eq!(hit_test_contact_table_header(&layout, 510.0), None);
    }

    #[test]
    fn test_contacts_toolbar_and_search_bar_hig() {
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

        // View options split button is exposed for window header bar
        let split_btn = page
            .view_options_split_button()
            .expect("view_options_split_button exists");
        assert_eq!(
            split_btn.icon_name(),
            Some(glib::GString::from("view-list-symbolic"))
        );

        // Contact action pill is created and initially hidden (no contact selected)
        let pill = page
            .imp()
            .contact_actions_pill
            .borrow()
            .clone()
            .expect("contact_actions_pill exists");
        assert!(
            !pill.is_visible(),
            "contact_actions_pill must be hidden when no contact is selected"
        );
        assert!(pill.has_css_class("contact-header-actions"));
        assert!(pill.has_css_class("mail-action-pill"));

        // Header action buttons use symbolic icons matching the mail action pill pattern
        let edit_btn = page.imp().edit_button.borrow().clone().expect("edit button exists");
        let email_btn = page.imp().email_button.borrow().clone().expect("email button exists");
        let delete_btn = page.imp().delete_button.borrow().clone().expect("delete button exists");

        let get_image_icon = |btn: &gtk::Button| {
            btn.child().and_downcast::<gtk::Image>().and_then(|img| img.icon_name().map(|s| s.to_string()))
        };

        assert_eq!(get_image_icon(&edit_btn), Some("document-edit-symbolic".to_string()));
        assert_eq!(get_image_icon(&email_btn), Some("mail-message-new-symbolic".to_string()));
        assert_eq!(get_image_icon(&delete_btn), Some("user-trash-symbolic".to_string()));

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

    #[test]
    fn test_contact_url_encode() {
        assert_eq!(super::url_encode("Berlin"), "Berlin");
        assert_eq!(super::url_encode("Brandenburger Tor, Berlin"), "Brandenburger+Tor%2C+Berlin");
        assert_eq!(super::url_encode("Hauptstraße 1, 80331 München"), "Hauptstra%C3%9Fe+1%2C+80331+M%C3%BCnchen");
    }

    #[test]
    fn test_contacts_view_stack_and_simple_list() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();
        let view_stack = page.imp().view_stack.borrow().clone().expect("view stack exists");
        assert_eq!(view_stack.visible_child_name().as_deref(), Some("simple"));
        assert!(page.imp().simple_list_scroll.borrow().is_some());
        assert!(page.imp().list_scroll.borrow().is_some());

        // Test switching views
        view_stack.set_visible_child_name("table");
        assert_eq!(view_stack.visible_child_name().as_deref(), Some("table"));
        view_stack.set_visible_child_name("simple");
        assert_eq!(view_stack.visible_child_name().as_deref(), Some("simple"));

        // Paned layout default position and min sizes
        let preview_paned = page.imp().contact_preview_paned.borrow().clone().expect("paned exists");
        assert!(preview_paned.position() >= 280);
    }

    #[test]
    fn test_contact_status_box_visibility_contract() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();
        let status_box = page.imp().contact_status_box.borrow().clone().expect("status box exists");
        let spinner = page.imp().contact_search_spinner.borrow().clone().expect("spinner exists");
        let empty_state = page.imp().contact_search_status.borrow().clone().expect("empty state exists");

        // Initially both empty_state and spinner are hidden, so status_box should be hidden
        assert!(!status_box.is_visible());

        // When spinner becomes visible, status_box becomes visible
        spinner.set_visible(true);
        assert!(status_box.is_visible());

        // When spinner hidden again, status_box becomes hidden
        spinner.set_visible(false);
        assert!(!status_box.is_visible());

        // When empty_state becomes visible, status_box becomes visible
        empty_state.set_visible(true);
        assert!(status_box.is_visible());

        // When empty_state hidden again, status_box becomes hidden
        empty_state.set_visible(false);
        assert!(!status_box.is_visible());
    }

    #[test]
    fn test_contacts_paned_clipping_sort_arrows_and_org_cleanup() {
        assert_eq!(super::clean_contact_org("privat;"), "privat");
        assert_eq!(super::clean_contact_org("Acme Corp;Engineering;"), "Acme Corp, Engineering");
        assert_eq!(super::clean_contact_org(";;"), "");

        let parsed = crate::services::vcard_service::VCardContact::parse_vcard_string(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Max Mustermann\r\nORG:privat;\r\nEND:VCARD\r\n",
        )
        .expect("parse vcard");
        assert_eq!(parsed.org, "privat");

        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        use adw::subclass::prelude::*;
        let page = ContactsPage::new();
        let preview_paned = page.imp().contact_preview_paned.borrow().clone().expect("preview paned");
        assert!(preview_paned.shrinks_start_child());
        assert!(preview_paned.shrinks_end_child());
        assert!(preview_paned.resizes_start_child());
        assert!(preview_paned.resizes_end_child());

        let list_container = preview_paned.start_child().expect("start child");
        let detail_box = preview_paned.end_child().expect("end child");
        assert_eq!(list_container.overflow(), gtk::Overflow::Hidden);
        assert_eq!(detail_box.overflow(), gtk::Overflow::Hidden);

        // Sort label uses 1:1 Mail View arrows (↑ / ↓)
        page.imp().contact_sort_col.set(ContactTableColumn::Name);
        page.imp().contact_sort_desc.set(false);
        assert!(page.contact_header_sort_label(ContactTableColumn::Name).ends_with(" ↑"));
        page.imp().contact_sort_desc.set(true);
        assert!(page.contact_header_sort_label(ContactTableColumn::Name).ends_with(" ↓"));
        assert!(!page.contact_header_sort_label(ContactTableColumn::Email).contains('↑'));
        assert!(!page.contact_header_sort_label(ContactTableColumn::Email).contains('↓'));

        // Populate contact_table_layout first, then resize columns to ensure no RefCell BorrowMutError panic
        let sample = record(
            "book-1",
            "uid-1",
            VCardContact::new("Max Mustermann", "max@example.test", "1234", "privat"),
        );
        page.update_contacts_table_layout(&[sample], None, &std::collections::HashSet::new());
        assert!(page.imp().contact_table_layout.borrow().is_some());
        page.apply_contact_column_width("name", 260);
        page.apply_contact_column_width("company", 210);
        assert_eq!(page.get_contact_column_width("name"), 260);
        assert_eq!(page.get_contact_column_width("company"), 210);
        if let Some(header) = page.imp().contact_table_header.borrow().as_ref() {
            assert_eq!(header.width_request(), -1);
        }

        // Desktop side-by-side invariant: enabling sidepanel keeps list_container and detail_box both visible in Table & Simple modes
        page.set_screen_mode(false, false);
        page.imp().is_detail_shown.set(true);
        page.set_view_mode(ContactViewMode::Table);
        page.set_preview_visible(true);
        assert!(list_container.is_visible());
        assert!(detail_box.is_visible());
        assert!(!page.is_detail_shown());

        page.set_view_mode(ContactViewMode::Simple);
        page.set_preview_visible(true);
        assert!(list_container.is_visible());
        assert!(detail_box.is_visible());
    }
}
