// SPDX-License-Identifier: GPL-3.0-or-later
//
// MemosPage — Dedicated Memos and Notes (VJOURNAL) manager view.
//
// Features:
// - Color-coded note entries
// - Memo list sidebar & detail text viewer
// - "+ Neue Notiz" memo creation button

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::Utc;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use crate::dialogs::event_dialog::AlarmEditor;
use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarStoreService, MemoAttachment, MemoEditorDraft, MemoRecord,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService,
    ECalSourceCapability, ECalSyncEvent, load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::memo_filter_service::{MemoCategoryFilter, MemoFilter, MemoSearchMode};
use crate::services::memo_surface_service::{
    MemoActionState, MemoIdentity, MemoPasteItem, MemoPreviewOrientation, MemoPreviewState,
    MemoSelection, build_clipboard_payload, build_forward_publish_payload, build_multi_export,
    build_print_document, plan_delete, plan_paste_into, preview_for_selection,
    validated_url_for_record,
};
use crate::services::reminder_service::{Alarm, replace_component_alarms};
use crate::services::settings_service::AppSettings;
use crate::views::accessibility::{
    announce_to_screen_reader, set_accessible_description, set_accessible_label,
    set_accessible_role,
};
use crate::views::calendar_page::calendar_source_color;
use crate::views::set_button_icon_with_fallback;

mod imp {
    use super::*;

    #[derive(Debug)]
    pub struct MemosPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub memo_category_filter: RefCell<Option<String>>,
        pub memo_category_model: RefCell<Option<gtk::StringList>>,
        pub memo_category_selector: RefCell<Option<gtk::DropDown>>,
        pub memo_source_filter: RefCell<Option<String>>,
        pub collapsed_memo_groups: RefCell<HashSet<String>>,
        pub memo_available_sources: RefCell<HashSet<String>>,
        pub memo_unavailable_sources: RefCell<HashSet<String>>,
        pub memo_writable_sources: RefCell<HashSet<String>>,
        pub memo_search_mode: Cell<MemoSearchMode>,
        pub memo_search_query: RefCell<String>,
        pub memo_selection: RefCell<MemoSelection>,
        pub memo_refresh_generation: Cell<u64>,
        pub memo_preview_state: RefCell<MemoPreviewState>,
        pub memo_list: RefCell<Option<gtk::ListBox>>,
        pub memo_table_header: RefCell<Option<gtk::DrawingArea>>,
        pub memo_table_canvas: RefCell<Option<gtk::DrawingArea>>,
        pub(crate) memo_table_layout: RefCell<Option<PreparedMemoTableLayout>>,
        pub memo_table_hovered_row: Cell<Option<usize>>,
        pub(crate) memo_table_hovered_header: Cell<Option<MemoTableColumn>>,
        pub memo_table_anchor_row: Cell<Option<usize>>,
        pub(crate) memo_sort: Cell<MemoSort>,
        pub memo_sort_descending: Cell<bool>,
        pub memo_records: RefCell<Option<Rc<RefCell<Vec<MemoRecord>>>>>,
        pub memo_empty_state: RefCell<Option<gtk::Label>>,
        pub new_memo_button: RefCell<Option<gtk::Button>>,
        pub save_memo_button: RefCell<Option<gtk::Button>>,
        pub delete_memo_button: RefCell<Option<gtk::Button>>,
        pub print_memo_button: RefCell<Option<gtk::Button>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub memo_preview_label: RefCell<Option<gtk::Label>>,
        pub memo_preview_box: RefCell<Option<gtk::Box>>,
        pub outer_paned: RefCell<Option<gtk::Paned>>,
        pub memo_preview_paned: RefCell<Option<gtk::Paned>>,
        pub memo_preview_position_restoring: Cell<bool>,
        pub memo_detail_stack: RefCell<Option<gtk::Stack>>,
        pub memo_surface_buttons: RefCell<Option<Vec<gtk::Button>>>,
        pub memo_source_list: RefCell<Option<gtk::ListBox>>,
        pub memo_source_selector: RefCell<crate::models::source_selector::SourceSelector>,
        pub hidden_memo_sources: RefCell<HashSet<String>>,
        pub memo_color_css_provider: RefCell<Option<gtk::CssProvider>>,
        pub memo_search_bar: RefCell<Option<gtk::SearchBar>>,
        pub toolbar: RefCell<Option<gtk::Widget>>,
        pub memo_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub memo_preview_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub is_mobile: Cell<bool>,
        pub is_tablet: Cell<bool>,
        pub is_detail_shown: Cell<bool>,
    }

    impl Default for MemosPage {
        fn default() -> Self {
            Self {
                is_mobile: Cell::new(false),
                is_tablet: Cell::new(false),
                is_detail_shown: Cell::new(false),
                ecal_monitor_cancel: RefCell::new(None),
                memo_category_filter: RefCell::new(None),
                memo_category_model: RefCell::new(None),
                memo_category_selector: RefCell::new(None),
                memo_source_filter: RefCell::new(None),
                collapsed_memo_groups: RefCell::new(HashSet::new()),
                memo_available_sources: RefCell::new(HashSet::new()),
                memo_unavailable_sources: RefCell::new(HashSet::new()),
                memo_writable_sources: RefCell::new(HashSet::new()),
                memo_search_mode: Cell::new(MemoSearchMode::default()),
                memo_search_query: RefCell::new(String::new()),
                memo_selection: RefCell::new(MemoSelection::default()),
                memo_refresh_generation: Cell::new(0),
                memo_preview_state: RefCell::new(MemoPreviewState::default()),
                memo_list: RefCell::new(None),
                memo_table_header: RefCell::new(None),
                memo_table_canvas: RefCell::new(None),
                memo_table_layout: RefCell::new(None),
                memo_table_hovered_row: Cell::new(None),
                memo_table_hovered_header: Cell::new(None),
                memo_table_anchor_row: Cell::new(None),
                memo_sort: Cell::new(MemoSort::default()),
                memo_sort_descending: Cell::new(true),
                memo_records: RefCell::new(None),
                memo_empty_state: RefCell::new(None),
                new_memo_button: RefCell::new(None),
                save_memo_button: RefCell::new(None),
                delete_memo_button: RefCell::new(None),
                print_memo_button: RefCell::new(None),
                search_entry: RefCell::new(None),
                memo_preview_label: RefCell::new(None),
                memo_preview_box: RefCell::new(None),
                outer_paned: RefCell::new(None),
                memo_preview_paned: RefCell::new(None),
                memo_preview_position_restoring: Cell::new(false),
                memo_detail_stack: RefCell::new(None),
                memo_surface_buttons: RefCell::new(None),
                memo_source_list: RefCell::new(None),
                memo_source_selector: RefCell::new(
                    crate::models::source_selector::SourceSelector::new(
                        crate::models::source_selector::SourceDomain::Memos,
                    ),
                ),
                hidden_memo_sources: RefCell::new(HashSet::new()),
                memo_color_css_provider: RefCell::new(None),
                memo_search_bar: RefCell::new(None),
                toolbar: RefCell::new(None),
                memo_sidebar_toggle: RefCell::new(None),
                memo_preview_toggle: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MemosPage {
        const NAME: &'static str = "MeridianMemosPage";
        type Type = super::MemosPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for MemosPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_vexpand(true);
            self.obj().set_hexpand(true);
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(cancel) = self.ecal_monitor_cancel.borrow_mut().take() {
                use std::sync::atomic::Ordering;
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MemosPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

glib::wrapper! {
    pub struct MemosPage(ObjectSubclass<imp::MemosPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MemoSort {
    #[default]
    Updated,
    Title,
    Source,
}

// --- ETable Memos Canvas Models and Drawing (Original GNOME Evolution Canvas parity) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoTableColumn {
    Color,
    Title,
    Source,
    Updated,
    Categories,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedMemoTableRow {
    pub(crate) memo: MemoRecord,
    pub(crate) identity: MemoIdentity,
    pub(crate) summary: String,
    pub(crate) source_display: String,
    pub(crate) updated: String,
    pub(crate) categories: Vec<String>,
    pub(crate) category_color: Option<String>,
    pub(crate) y: f64,
    pub(crate) height: f64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedMemoTableLayout {
    pub(crate) rows: Vec<PreparedMemoTableRow>,
    pub(crate) total_height: f64,
    pub(crate) width: f64,
    pub(crate) col_color_w: f64,
    pub(crate) col_title_w: f64,
    pub(crate) col_source_w: f64,
    pub(crate) col_updated_w: f64,
    pub(crate) col_categories_w: f64,
}

fn truncate_cairo_text(cr: &gtk::cairo::Context, text: &str, max_width: f64) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if let Ok(extents) = cr.text_extents(text) {
        if extents.width() <= max_width {
            return text.to_string();
        }
    }
    let ellipsis = "…";
    let ellipsis_w = cr.text_extents(ellipsis).map(|e| e.width()).unwrap_or(10.0);
    let target_w = max_width - ellipsis_w;
    if target_w <= 0.0 {
        return ellipsis.to_string();
    }
    let mut truncated = String::new();
    for ch in text.chars() {
        truncated.push(ch);
        if let Ok(extents) = cr.text_extents(&truncated) {
            if extents.width() > target_w {
                truncated.pop();
                break;
            }
        }
    }
    truncated.push_str(ellipsis);
    truncated
}

fn parse_hex_color(hex: &str) -> Option<(f64, f64, f64)> {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&clean[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&clean[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&clean[4..6], 16).ok()? as f64 / 255.0;
    Some((r, g, b))
}

pub(crate) fn prepare_memo_table_layout(
    memos: &[MemoRecord],
    total_width: f64,
) -> PreparedMemoTableLayout {
    let row_height = 32.0;
    let col_color_w = 28.0;
    let col_source_w = 140.0;
    let col_updated_w = 120.0;
    let col_categories_w = 120.0;
    let col_title_w = (total_width - col_color_w - col_source_w - col_updated_w - col_categories_w - 16.0).max(160.0);

    let mut rows = Vec::with_capacity(memos.len());
    let mut current_y = 0.0;
    let cat_store = CalendarCategoryStore::default();

    for memo in memos {
        let summary = if memo.summary.trim().is_empty() {
            crate::i18n::gettext("(Untitled)")
        } else {
            memo.summary.clone()
        };
        let category_color = cat_store.color_for_component(&memo.component);
        let categories_str = component_categories(&memo.component);
        let categories = categories_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();

        rows.push(PreparedMemoTableRow {
            identity: MemoIdentity::from(memo),
            memo: memo.clone(),
            summary,
            source_display: memo.source_display_name.clone(),
            updated: memo.updated.clone(),
            categories,
            category_color,
            y: current_y,
            height: row_height,
        });

        current_y += row_height;
    }

    PreparedMemoTableLayout {
        rows,
        total_height: current_y,
        width: total_width,
        col_color_w,
        col_title_w,
        col_source_w,
        col_updated_w,
        col_categories_w,
    }
}

pub(crate) fn hit_test_memo_table_header(
    layout: &PreparedMemoTableLayout,
    x: f64,
) -> Option<MemoTableColumn> {
    if x < 0.0 || x > layout.width {
        return None;
    }
    if x < layout.col_color_w {
        Some(MemoTableColumn::Color)
    } else if x < layout.col_color_w + layout.col_title_w {
        Some(MemoTableColumn::Title)
    } else if x < layout.col_color_w + layout.col_title_w + layout.col_source_w {
        Some(MemoTableColumn::Source)
    } else if x < layout.col_color_w + layout.col_title_w + layout.col_source_w + layout.col_updated_w {
        Some(MemoTableColumn::Updated)
    } else if x < layout.col_color_w + layout.col_title_w + layout.col_source_w + layout.col_updated_w + layout.col_categories_w {
        Some(MemoTableColumn::Categories)
    } else {
        None
    }
}

pub(crate) fn hit_test_memo_table(
    layout: &PreparedMemoTableLayout,
    y: f64,
) -> Option<usize> {
    if layout.rows.is_empty() || y < 0.0 || y >= layout.total_height {
        return None;
    }
    let row_idx = (y / 32.0).floor() as usize;
    if row_idx < layout.rows.len() {
        Some(row_idx)
    } else {
        None
    }
}

pub(crate) fn draw_memo_table_header(
    cr: &gtk::cairo::Context,
    layout: &PreparedMemoTableLayout,
    width: f64,
    height: f64,
    sort: MemoSort,
    descending: bool,
    hovered: Option<MemoTableColumn>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgba(0.18, 0.18, 0.18, 0.9);
    } else {
        cr.set_source_rgba(0.96, 0.96, 0.96, 0.9);
    }
    cr.rectangle(0.0, 0.0, width, height);
    cr.fill().ok();

    cr.set_line_width(1.0);
    if is_dark {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.1);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.1);
    }
    cr.move_to(0.0, height - 0.5);
    cr.line_to(width, height - 0.5);
    cr.stroke().ok();

    let col_title = crate::i18n::gettext("Title");
    let col_source = crate::i18n::gettext("Notebook");
    let col_updated = crate::i18n::gettext("Updated");
    let col_categories = crate::i18n::gettext("Categories");
    let columns = [
        (MemoTableColumn::Title, col_title.as_str(), layout.col_color_w, layout.col_title_w),
        (MemoTableColumn::Source, col_source.as_str(), layout.col_color_w + layout.col_title_w, layout.col_source_w),
        (MemoTableColumn::Updated, col_updated.as_str(), layout.col_color_w + layout.col_title_w + layout.col_source_w, layout.col_updated_w),
        (MemoTableColumn::Categories, col_categories.as_str(), layout.col_color_w + layout.col_title_w + layout.col_source_w + layout.col_updated_w, layout.col_categories_w),
    ];

    cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Bold);
    cr.set_font_size(11.0);

    for (col, label, col_x, col_w) in columns {
        if hovered == Some(col) {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
            }
            cr.rectangle(col_x, 0.0, col_w, height - 1.0);
            cr.fill().ok();
        }

        if is_dark {
            cr.set_source_rgba(0.75, 0.75, 0.75, 1.0);
        } else {
            cr.set_source_rgba(0.35, 0.35, 0.35, 1.0);
        }

        let is_sorted = match col {
            MemoTableColumn::Title => sort == MemoSort::Title,
            MemoTableColumn::Source => sort == MemoSort::Source,
            MemoTableColumn::Updated => sort == MemoSort::Updated,
            _ => false,
        };

        let display_text = truncate_cairo_text(cr, label, col_w - 24.0);
        cr.move_to(col_x + 8.0, 18.0);
        cr.show_text(&display_text).ok();

        if is_sorted {
            let arrow_x = col_x + 8.0 + cr.text_extents(&display_text).map(|e| e.width()).unwrap_or(30.0) + 6.0;
            let arrow_y = 14.0;
            cr.set_line_width(1.2);
            if is_dark {
                cr.set_source_rgba(0.9, 0.9, 0.9, 0.9);
            } else {
                cr.set_source_rgba(0.2, 0.2, 0.2, 0.9);
            }
            if descending {
                cr.move_to(arrow_x - 3.5, arrow_y - 2.0);
                cr.line_to(arrow_x + 3.5, arrow_y - 2.0);
                cr.line_to(arrow_x, arrow_y + 3.0);
                cr.close_path();
            } else {
                cr.move_to(arrow_x - 3.5, arrow_y + 2.0);
                cr.line_to(arrow_x + 3.5, arrow_y + 2.0);
                cr.line_to(arrow_x, arrow_y - 3.0);
                cr.close_path();
            }
            cr.fill().ok();
        }

        cr.set_line_width(1.0);
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
        }
        cr.move_to(col_x + col_w - 0.5, 4.0);
        cr.line_to(col_x + col_w - 0.5, height - 5.0);
        cr.stroke().ok();
    }
}

pub(crate) fn draw_memo_table_canvas(
    cr: &gtk::cairo::Context,
    layout: &PreparedMemoTableLayout,
    selection: &MemoSelection,
    width: f64,
    height: f64,
    hovered_row: Option<usize>,
    is_dark: bool,
) {
    if is_dark {
        cr.set_source_rgb(0.12, 0.12, 0.12);
    } else {
        cr.set_source_rgb(1.0, 1.0, 1.0);
    }
    cr.paint().ok();

    let (clip_y1, clip_y2) = match cr.clip_extents() {
        Ok((_, y1, _, y2)) => (y1, y2),
        Err(_) => (0.0, height),
    };
    let row_h = 34.0;
    let start_idx = ((clip_y1 / row_h).floor().max(0.0) as usize).min(layout.rows.len());
    let end_idx = (((clip_y2 / row_h).ceil().max(0.0) as usize) + 1).min(layout.rows.len());

    for idx in start_idx..end_idx {
        let row = &layout.rows[idx];
        if row.y + row.height < clip_y1 || row.y > clip_y2 {
            continue;
        }

        let is_selected = selection.contains(&row.identity);
        let is_hovered = hovered_row == Some(idx);

        if is_selected {
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.25);
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
            cr.set_source_rgba(0.2, 0.5, 0.9, 0.4);
            cr.set_line_width(1.0);
            cr.rectangle(0.5, row.y + 0.5, width - 1.0, row.height - 1.0);
            cr.stroke().ok();
        } else if is_hovered {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.04);
            }
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
        } else if idx % 2 == 1 {
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.015);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.015);
            }
            cr.rectangle(0.0, row.y, width, row.height);
            cr.fill().ok();
        }

        // Column 0: Category Color Dot
        let dot_cx = layout.col_color_w / 2.0;
        let dot_cy = row.y + row.height / 2.0;
        if let Some(color_hex) = row.category_color.as_deref().and_then(parse_hex_color) {
            cr.set_source_rgb(color_hex.0, color_hex.1, color_hex.2);
            cr.arc(dot_cx, dot_cy, 4.5, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        } else {
            if is_dark {
                cr.set_source_rgba(0.6, 0.6, 0.6, 0.4);
            } else {
                cr.set_source_rgba(0.4, 0.4, 0.4, 0.4);
            }
            cr.arc(dot_cx, dot_cy, 3.5, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }

        let text_baseline = row.y + row.height / 2.0 + 4.0;

        // Column 1: Title
        let title_x = layout.col_color_w + 8.0;
        cr.select_font_face("Sans", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
        cr.set_font_size(12.0);
        if is_dark {
            cr.set_source_rgba(0.92, 0.92, 0.92, 1.0);
        } else {
            cr.set_source_rgba(0.12, 0.12, 0.12, 1.0);
        }
        let display_title = truncate_cairo_text(cr, &row.summary, layout.col_title_w - 16.0);
        cr.move_to(title_x, text_baseline);
        cr.show_text(&display_title).ok();

        // Column 2: Source Notebook
        let source_x = layout.col_color_w + layout.col_title_w + 8.0;
        cr.set_font_size(11.0);
        if is_dark {
            cr.set_source_rgba(0.65, 0.65, 0.65, 0.8);
        } else {
            cr.set_source_rgba(0.45, 0.45, 0.45, 0.8);
        }
        let display_source = truncate_cairo_text(cr, &row.source_display, layout.col_source_w - 16.0);
        cr.move_to(source_x, text_baseline);
        cr.show_text(&display_source).ok();

        // Column 3: Updated Date
        let updated_x = source_x + layout.col_source_w;
        let display_updated = truncate_cairo_text(cr, &row.updated, layout.col_updated_w - 16.0);
        cr.move_to(updated_x, text_baseline);
        cr.show_text(&display_updated).ok();

        // Column 4: Categories
        let cat_x = updated_x + layout.col_updated_w;
        if !row.categories.is_empty() {
            let cat_str = row.categories.join(", ");
            let display_cat = truncate_cairo_text(cr, &cat_str, layout.col_categories_w - 16.0);
            cr.move_to(cat_x, text_baseline);
            cr.show_text(&display_cat).ok();
        }

        // Horizontal separator line
        cr.set_line_width(1.0);
        if is_dark {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.05);
        } else {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.05);
        }
        let line_y = (row.y + row.height).floor() + 0.5;
        cr.move_to(0.0, line_y);
        cr.line_to(width, line_y);
        cr.stroke().ok();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoSourceProjection {
    uid: String,
    display_name: String,
    connected: bool,
    available: bool,
    writable: bool,
}

fn project_memo_source_capabilities(
    capabilities: Vec<ECalSourceCapability>,
) -> Vec<MemoSourceProjection> {
    let mut seen = HashSet::new();
    let mut sources = capabilities
        .into_iter()
        .filter_map(|source| {
            let uid = source.source_uid.trim().to_string();
            (!uid.is_empty() && seen.insert(uid.clone())).then(|| MemoSourceProjection {
                uid,
                display_name: source.display_name,
                connected: source.connected,
                available: source.available && source.connected,
                writable: source.available && source.connected && source.writable,
            })
        })
        .collect::<Vec<_>>();
    sources.sort_by_cached_key(|source| (source.display_name.to_lowercase(), source.uid.clone()));
    sources
}

fn memo_source_projection_label(source: &MemoSourceProjection) -> String {
    let name = (!source.display_name.trim().is_empty())
        .then_some(source.display_name.as_str())
        .unwrap_or(source.uid.as_str());
    if !source.connected {
        format!("{} · {name}", crate::i18n::gettext("Unreachable"))
    } else if !source.available {
        format!("{} · {name}", crate::i18n::gettext("Unavailable"))
    } else if !source.writable {
        format!("{} · {name}", crate::i18n::gettext("Read-Only"))
    } else {
        name.to_string()
    }
}

impl MemoSort {
    fn from_setting(value: &str) -> Self {
        match value {
            "title" => Self::Title,
            "source" => Self::Source,
            _ => Self::Updated,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Title => "title",
            Self::Source => "source",
        }
    }
}

impl MemosPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
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

    /// Trigger the page-local memo editor from the shared Evolution toolbar.
    pub fn present_new_memo(&self) {
        if let Some(button) = self.imp().new_memo_button.borrow().as_ref() {
            button.emit_clicked();
        }
    }

    pub fn outer_paned(&self) -> Option<gtk::Paned> {
        self.imp().outer_paned.borrow().clone()
    }

    pub fn is_preview_visible(&self) -> bool {
        if let Some(preview_paned) = self.imp().memo_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                return detail_box.is_visible();
            }
        }
        AppSettings::try_new()
            .map(|s| s.memo_preview_visible())
            .unwrap_or(true)
    }

    pub fn set_preview_visible(&self, visible: bool) {
        if let Some(preview_paned) = self.imp().memo_preview_paned.borrow().as_ref() {
            if let Some(detail_box) = preview_paned.end_child() {
                detail_box.set_visible(visible);
            }
        }
        if let Some(toggle) = self.imp().memo_preview_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_memo_preview_visible(visible);
        }
        if let Some(win) = crate::window::find_meridian_window(self) {
            win.sync_right_sidebar_toggle();
        }
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
                    .unwrap_or(260)
                    .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
                if paned.position() < 180 {
                    paned.set_position(saved_pos);
                }
            }
        }
        if let Some(toggle) = self.imp().memo_sidebar_toggle.borrow().as_ref() {
            if toggle.is_active() != visible {
                toggle.set_active(visible);
            }
        }
    }

    pub(crate) fn set_screen_mode(&self, is_mobile: bool, is_tablet: bool) {
        let imp = self.imp();
        imp.is_mobile.set(is_mobile);
        imp.is_tablet.set(is_tablet);
        self.sync_memos_layout_visibility();
        self.sync_toolbar_button_labels();
    }

    pub(crate) fn apply_toolbar_button_modes(&self, is_compact: bool) {
        let imp = self.imp();
        let save_text = crate::i18n::gettext("Save");
        let delete_text = crate::i18n::gettext("Delete");
        let buttons: [(&RefCell<Option<gtk::Button>>, &str, &str); 2] = [
            (&imp.save_memo_button, &save_text, "document-save-symbolic"),
            (&imp.delete_memo_button, &delete_text, "user-trash-symbolic"),
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

    pub(crate) fn is_detail_shown(&self) -> bool {
        self.imp().is_detail_shown.get()
    }

    pub(crate) fn selected_memo_title(&self) -> Option<String> {
        let imp = self.imp();
        let selection = imp.memo_selection.borrow();
        let first_sel = selection.selected().first()?;
        let records = imp.memo_records.borrow().as_ref()?.clone();
        let list = records.borrow();
        for memo in list.iter() {
            if memo.uid == first_sel.uid && memo.source_uid == first_sel.source_uid {
                let summary = memo.summary.trim().to_string();
                return if summary.is_empty() {
                    Some(crate::i18n::gettext("Memo"))
                } else {
                    Some(summary)
                };
            }
        }
        None
    }

    pub(crate) fn can_navigate_back(&self) -> bool {
        let imp = self.imp();
        imp.is_mobile.get() && imp.is_detail_shown.get()
    }

    pub(crate) fn navigate_back(&self) -> bool {
        let imp = self.imp();
        if imp.is_mobile.get() && imp.is_detail_shown.get() {
            imp.is_detail_shown.set(false);
            self.sync_memos_layout_visibility();
            if let Some(win) = crate::window::find_meridian_window(self) {
                win.sync_back_button();
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn sync_memos_layout_visibility(&self) {
        let imp = self.imp();
        let is_mobile = imp.is_mobile.get();
        let is_tablet = imp.is_tablet.get();
        let Some(outer_paned) = imp.outer_paned.borrow().clone() else { return; };
        let Some(preview_paned) = imp.memo_preview_paned.borrow().clone() else { return; };

        let sidebar = outer_paned.start_child();
        let list_container = preview_paned.start_child();
        let detail_box = preview_paned.end_child();

        if is_mobile {
            if let Some(sb) = sidebar.as_ref() {
                sb.set_visible(false);
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
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_visible(true);
            }
        } else {
            if let Some(lc) = list_container.as_ref() {
                lc.set_visible(true);
            }
            if let Some(db) = detail_box.as_ref() {
                db.set_visible(true);
            }
        }

        if let Some(toolbar) = imp.toolbar.borrow().as_ref() {
            if is_mobile {
                toolbar.set_visible(false);
            } else {
                let show_tb = AppSettings::try_new()
                    .map(|s| s.memo_show_toolbar())
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
            settings.set_memo_show_toolbar(visible);
        }
    }

    pub fn memo_preview_paned(&self) -> Option<gtk::Paned> {
        self.imp().memo_preview_paned.borrow().clone()
    }

    pub fn memo_detail_stack(&self) -> Option<gtk::Stack> {
        self.imp().memo_detail_stack.borrow().clone()
    }
}

fn memo_paned_orientation(orientation: MemoPreviewOrientation) -> gtk::Orientation {
    match orientation {
        MemoPreviewOrientation::Classic => gtk::Orientation::Vertical,
        MemoPreviewOrientation::Vertical => gtk::Orientation::Horizontal,
    }
}

fn apply_memo_preview_paned_layout(page: &MemosPage, orientation: MemoPreviewOrientation) {
    let Some(paned) = page.imp().memo_preview_paned.borrow().as_ref().cloned() else {
        return;
    };
    let position = AppSettings::try_new()
        .map(|settings| settings.memo_preview_position(orientation.setting_value()))
        .unwrap_or_else(|| orientation.default_position());
    page.imp().memo_preview_position_restoring.set(true);
    let gtk_orientation = memo_paned_orientation(orientation);
    if let Some(start) = paned.start_child() {
        if gtk_orientation == gtk::Orientation::Horizontal {
            start.set_size_request(240, -1);
        } else {
            start.set_size_request(-1, 160);
        }
    }
    if let Some(end) = paned.end_child() {
        if gtk_orientation == gtk::Orientation::Horizontal {
            end.set_size_request(220, -1);
        } else {
            end.set_size_request(-1, 160);
        }
    }
    paned.set_orientation(gtk_orientation);
    paned.set_position(position);
    page.imp().memo_preview_position_restoring.set(false);
}

impl MemosPage {
    pub fn sidebar_toggle(&self) -> Option<gtk::ToggleButton> {
        self.imp().memo_sidebar_toggle.borrow().clone()
    }

    pub fn preview_toggle(&self) -> Option<gtk::ToggleButton> {
        self.imp().memo_preview_toggle.borrow().clone()
    }

    pub fn set_sidebar_position(&self, pos: i32) {
        if let Some(paned) = self.imp().outer_paned.borrow().as_ref() {
            if (paned.position() - pos).abs() > 1 {
                paned.set_position(pos);
            }
        }
    }

    pub fn search_bar(&self) -> Option<gtk::SearchBar> {
        self.imp().memo_search_bar.borrow().clone()
    }

    pub(crate) fn focus_search(&self) {
        if let Some(bar) = self.imp().memo_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn is_search_active(&self) -> bool {
        self.imp()
            .memo_search_bar
            .borrow()
            .as_ref()
            .map(|b| b.is_search_mode())
            .unwrap_or(false)
    }

    pub(crate) fn close_search(&self) {
        if let Some(bar) = self.imp().memo_search_bar.borrow().as_ref() {
            bar.set_search_mode(false);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(bar) = self.imp().memo_search_bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn refresh_hidden_memo_sources(&self) {
        if let Some(settings) = AppSettings::try_new() {
            let hidden = settings.memo_hidden_sources().into_iter().collect::<HashSet<_>>();
            *self.imp().hidden_memo_sources.borrow_mut() = hidden;
            self.imp().memo_source_selector.borrow_mut().load_from_settings(&settings);
        }
    }

    pub(crate) fn refresh_active_memo_list(&self) {
        let (Some(memo_list), Some(memo_records), Some(empty_state)) = (
            self.imp().memo_list.borrow().as_ref().cloned(),
            self.imp().memo_records.borrow().as_ref().cloned(),
            self.imp().memo_empty_state.borrow().as_ref().cloned(),
        ) else {
            return;
        };
        refresh_memo_category_model(self, &memo_list);
        refresh_memo_filter_view(self, &memo_list, &memo_records, &empty_state);
    }

    pub(crate) fn remove_memo_from_cache(&self, source_uid: &str, uid: &str) {
        let (Some(memo_list), Some(memo_records), Some(empty_state)) = (
            self.imp().memo_list.borrow().as_ref().cloned(),
            self.imp().memo_records.borrow().as_ref().cloned(),
            self.imp().memo_empty_state.borrow().as_ref().cloned(),
        ) else {
            return;
        };
        if remove_memo_row(&memo_list, Rc::clone(&memo_records), source_uid, uid) {
            let generation = self.imp().memo_refresh_generation.get().wrapping_add(1);
            self.imp().memo_refresh_generation.set(generation);
            refresh_memo_category_model(self, &memo_list);
            refresh_memo_filter_view(self, &memo_list, &memo_records, &empty_state);
        }
    }

    pub(crate) fn select_exclusive_memo_source(&self, uid: &str) {
        {
            let mut sel = self.imp().memo_source_selector.borrow_mut();
            sel.select_exclusive(uid);
            if let Some(settings) = AppSettings::try_new() {
                sel.save_to_settings(&settings);
            }
            *self.imp().hidden_memo_sources.borrow_mut() =
                sel.hidden_uids().into_iter().collect();
        }
        self.render_memo_sidebar();
        self.refresh_active_memo_list();
    }

    pub(crate) fn select_all_memo_sources(&self) {
        {
            let mut sel = self.imp().memo_source_selector.borrow_mut();
            sel.select_all();
            if let Some(settings) = AppSettings::try_new() {
                sel.save_to_settings(&settings);
            }
            *self.imp().hidden_memo_sources.borrow_mut() =
                sel.hidden_uids().into_iter().collect();
        }
        self.render_memo_sidebar();
        self.refresh_active_memo_list();
    }

    pub(crate) fn set_default_memo_source(&self, uid: &str) {
        {
            let mut sel = self.imp().memo_source_selector.borrow_mut();
            sel.set_primary(uid);
            if let Some(settings) = AppSettings::try_new() {
                sel.save_to_settings(&settings);
                settings.set_default_memo_source_uid(uid);
            }
        }
        self.render_memo_sidebar();
    }

    pub(crate) fn present_memo_source_context_menu(
        &self,
        parent: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
        source_uid: &str,
        display_name: &str,
        color: &str,
        is_writable: bool,
        is_primary: bool,
    ) {
        let page_weak = self.downgrade();
        let uid_excl = source_uid.to_string();
        let on_exclusive = {
            let page_weak = page_weak.clone();
            let uid = uid_excl;
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.select_exclusive_memo_source(&uid);
                }
            }
        };

        let on_select_all = {
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.select_all_memo_sources();
                }
            }
        };

        let uid_def = source_uid.to_string();
        let on_set_default = {
            let page_weak = page_weak.clone();
            let uid = uid_def;
            move || {
                if let Some(page) = page_weak.upgrade() {
                    page.set_default_memo_source(&uid);
                }
            }
        };

        let uid_props = source_uid.to_string();
        let name_props = display_name.to_string();
        let col_props = color.to_string();
        let on_properties = Some({
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    let notebook_label = crate::i18n::gettext("Notebook");
                    let dialog = crate::dialogs::SourcePropertiesDialog::new(
                        &uid_props,
                        &name_props,
                        &notebook_label,
                        &col_props,
                        is_writable,
                        is_primary,
                    );
                    let page_weak_save = page.downgrade();
                    dialog.set_on_save(move |update| {
                        if let Some(page) = page_weak_save.upgrade() {
                            {
                                let mut sel = page.imp().memo_source_selector.borrow_mut();
                                sel.update_source_metadata(
                                    &update.source_uid,
                                    &update.display_name,
                                    Some(&update.color),
                                    update.is_default,
                                );
                                if let Some(settings) = AppSettings::try_new() {
                                    sel.save_to_settings(&settings);
                                    if update.is_default {
                                        settings.set_default_memo_source_uid(&update.source_uid);
                                    }
                                }
                            }
                            page.render_memo_sidebar();
                        }
                    });
                    dialog.present(Some(&page));
                }
            }
        });

        let on_manage_groups = Some({
            let page_weak = page_weak.clone();
            move || {
                if let Some(page) = page_weak.upgrade() {
                    let root_window = page.root().and_downcast::<gtk::Window>();
                    let current_sel = page.imp().memo_source_selector.borrow().clone();
                    let page_weak_save = page.downgrade();
                    let notebook_label = crate::i18n::gettext("Notebook");
                    let dialog = crate::dialogs::ManageGroupsDialog::new(
                        root_window.as_ref(),
                        &notebook_label,
                        &current_sel,
                        move |updated_selector| {
                            if let Some(page) = page_weak_save.upgrade() {
                                {
                                    let mut sel = page.imp().memo_source_selector.borrow_mut();
                                    *sel = updated_selector;
                                    sel.deselect_sources_in_hidden_groups();
                                    if let Some(settings) = AppSettings::try_new() {
                                        sel.save_to_settings(&settings);
                                    }
                                    *page.imp().hidden_memo_sources.borrow_mut() =
                                        sel.hidden_uids().into_iter().collect();
                                }
                                page.render_memo_sidebar();
                            }
                        },
                    );
                    dialog.present();
                }
            }
        });

        let notebook_label = crate::i18n::gettext("Notebook");
        let popover = crate::models::source_selector::create_source_context_popover(
            is_primary,
            &notebook_label,
            on_exclusive,
            on_select_all,
            on_set_default,
            on_properties,
            on_manage_groups,
        );
        popover.set_parent(parent);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || {
                p.unparent();
            });
        });
        popover.present();
    }

    pub(crate) fn render_memo_sidebar(&self) {
        let Some(source_list) = self.imp().memo_source_list.borrow().as_ref().cloned() else {
            return;
        };
        while let Some(child) = source_list.first_child() {
            source_list.remove(&child);
        }

        let selector = self.imp().memo_source_selector.borrow().clone();
        let collapsed_groups = self.imp().collapsed_memo_groups.borrow().clone();

        // Generate and install dynamic CSS rules for memo source colors on checkboxes
        let mut unique_colors = std::collections::HashSet::new();
        for s in selector.visible_sources() {
            if let Some(ref c) = s.color {
                unique_colors.insert(c.clone());
            }
        }
        unique_colors.insert("#3584e4".to_string());

        let mut css_rules = String::new();
        for hex in unique_colors {
            let clean: String = hex
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            css_rules.push_str(&format!(
                "checkbutton.cal-check-{clean} {{ \
                     --accent-bg-color: {hex}; \
                     --accent-color: {hex}; \
                 }} \
                 checkbutton.cal-check-{clean} check:not(:checked) {{ \
                     box-shadow: inset 0 0 0 2px {hex}; \
                 }}\n"
            ));
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let mut prov_slot = self.imp().memo_color_css_provider.borrow_mut();
            if let Some(ref provider) = *prov_slot {
                provider.load_from_string(&css_rules);
            } else {
                let provider = gtk::CssProvider::new();
                provider.load_from_string(&css_rules);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 50,
                );
                *prov_slot = Some(provider);
            }
        }

        for group in selector.ordered_groups() {
            if group.is_hidden {
                continue;
            }
            let group_sources: Vec<_> = selector
                .visible_sources()
                .into_iter()
                .filter(|s| s.group_name == group.name)
                .collect();

            if group_sources.is_empty() && !group.is_custom {
                continue;
            }

            let is_collapsed = collapsed_groups.contains(&group.name);

            // Group header row
            let group_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .margin_start(0)
                .margin_end(6)
                .valign(gtk::Align::Center)
                .css_classes(vec![
                    "calendar-sidebar-row",
                    "calendar-sidebar-group-row",
                    "sidebar-account-row",
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
                .label(&group.name)
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

            let page_weak_for_group = self.downgrade();
            let group_name_for_toggle = group.name.clone();
            let group_gesture = gtk::GestureClick::new();
            group_gesture.connect_released(move |_, _, _, _| {
                let Some(page) = page_weak_for_group.upgrade() else {
                    return;
                };
                let mut collapsed = page.imp().collapsed_memo_groups.borrow_mut();
                if collapsed.contains(&group_name_for_toggle) {
                    collapsed.remove(&group_name_for_toggle);
                } else {
                    collapsed.insert(group_name_for_toggle.clone());
                }
                drop(collapsed);
                page.render_memo_sidebar();
            });
            group_row.add_controller(group_gesture);
            source_list.append(&group_row);

            if is_collapsed {
                continue;
            }

            for item in group_sources {
                let source_uid = item.uid.clone();
                let is_primary = selector.is_primary(&source_uid);
                let color_hex = item.color.clone().unwrap_or_else(|| "#3584e4".to_string());

                let child_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .margin_start(14)
                    .margin_end(6)
                    .css_classes(vec!["calendar-sidebar-row", "calendar-sidebar-child-row"])
                    .build();

                let clean_color: String = color_hex
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                let color_class = format!("cal-check-{}", clean_color);

                let check = gtk::CheckButton::builder()
                    .active(item.enabled && selector.is_selected(&source_uid))
                    .sensitive(item.enabled)
                    .valign(gtk::Align::Center)
                    .tooltip_text(&crate::i18n::gettext("Show/hide memo list in views"))
                    .css_classes(vec![color_class.as_str()])
                    .build();

                let page_weak_for_toggle = self.downgrade();
                let source_uid_for_toggle = source_uid.clone();
                check.connect_toggled(move |check| {
                    let Some(page) = page_weak_for_toggle.upgrade() else {
                        return;
                    };
                    {
                        let mut sel = page.imp().memo_source_selector.borrow_mut();
                        sel.set_selected(&source_uid_for_toggle, check.is_active());
                        if let Some(settings) = AppSettings::try_new() {
                            sel.save_to_settings(&settings);
                        }
                        *page.imp().hidden_memo_sources.borrow_mut() =
                            sel.hidden_uids().into_iter().collect();
                    }
                    page.refresh_active_memo_list();
                });
                child_box.append(&check);

                let label = gtk::Label::builder()
                    .label(&item.display_name)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build();
                child_box.append(&label);

                if is_primary {
                    let primary_ind =
                        crate::models::source_selector::create_primary_indicator();
                    primary_ind.set_valign(gtk::Align::Center);
                    child_box.append(&primary_ind);
                }

                if !item.enabled {
                    let sync_error_icon =
                        gtk::Image::from_icon_name("dialog-warning-symbolic");
                    sync_error_icon.add_css_class("warning");
                    sync_error_icon.set_tooltip_text(Some(&crate::i18n::gettext("Memo list not synchronized or offline")));
                    sync_error_icon.set_valign(gtk::Align::Center);
                    child_box.append(&sync_error_icon);
                }

                let row = gtk::ListBoxRow::builder()
                    .child(&child_box)
                    .activatable(true)
                    .selectable(true)
                    .css_classes(vec!["calendar-sidebar-child-row"])
                    .build();

                let page_weak_for_menu = self.downgrade();
                let source_uid_for_menu = source_uid.clone();
                let display_name_for_menu = item.display_name.clone();
                let color_for_menu = color_hex.clone();
                let is_writable = item.writable;
                let source_list_weak = source_list.downgrade();

                let right_click = gtk::GestureClick::builder()
                    .button(gdk::BUTTON_SECONDARY)
                    .build();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let (Some(page), Some(list)) =
                        (page_weak_for_menu.upgrade(), source_list_weak.upgrade())
                    else {
                        return;
                    };
                    page.present_memo_source_context_menu(
                        &list,
                        x,
                        y,
                        &source_uid_for_menu,
                        &display_name_for_menu,
                        &color_for_menu,
                        is_writable,
                        is_primary,
                    );
                });
                row.add_controller(right_click);

                source_list.append(&row);
            }
        }

        self.refresh_active_memo_list();
    }

    fn apply_memo_sources(&self, sources: Vec<MemoSourceProjection>) {
        let mut items = Vec::new();
        let mut available_sources = HashSet::new();
        let mut unavailable_sources = HashSet::new();
        let mut writable_sources = HashSet::new();
        for source in sources {
            let color = calendar_source_color(&source.uid, &source.display_name);
            let parent_name = crate::services::eds_service::resolve_source_parent_name(&source.uid);
            let group = crate::services::eds_service::canonical_group_name(&parent_name);
            if source.available {
                available_sources.insert(source.uid.clone());
            } else {
                unavailable_sources.insert(source.uid.clone());
            }
            if source.writable {
                writable_sources.insert(source.uid.clone());
            }
            items.push(
                crate::models::source_selector::SourceSelectorItem::new(
                    &source.uid,
                    &source.display_name,
                    "memo",
                    source.available,
                    source.writable,
                )
                .with_color(color)
                .with_group(group),
            );
        }
        for collection in crate::services::eds_service::load_configured_account_collections() {
            if !collection.calendar_enabled {
                continue;
            }
            let account_name = collection.display_name.clone();
            let already_represented = items.iter().any(|item| {
                item.group_name == account_name
                    || item.uid == collection.uid
                    || item.uid == format!("account-memos-{}", collection.uid)
            });
            if !already_represented {
                let memo_uid = format!("account-memos-{}", collection.uid);
                let color = calendar_source_color(&collection.uid, &account_name);
                available_sources.insert(memo_uid.clone());
                writable_sources.insert(memo_uid.clone());
                let memos_name = crate::i18n::gettext("Memos");
                items.push(
                    crate::models::source_selector::SourceSelectorItem::new(
                        &memo_uid,
                        &memos_name,
                        "memo",
                        true,
                        true,
                    )
                    .with_color(color)
                    .with_group(account_name),
                );
            }
        }

        if CalendarStoreService::new().is_writable() && !items.iter().any(|i| i.uid == "local-calendar") {
            let local_memos_name = crate::i18n::gettext("Local Memo List");
            let on_this_computer = crate::i18n::gettext("On This Computer");
            let color = calendar_source_color("local-calendar", &local_memos_name);
            available_sources.insert("local-calendar".to_string());
            writable_sources.insert("local-calendar".to_string());
            items.push(
                crate::models::source_selector::SourceSelectorItem::new(
                    "local-calendar",
                    &local_memos_name,
                    "local",
                    true,
                    true,
                )
                .with_color(color)
                .with_group(on_this_computer),
            );
        }

        self.imp().memo_available_sources.replace(available_sources);
        self.imp().memo_unavailable_sources.replace(unavailable_sources);
        self.imp().memo_writable_sources.replace(writable_sources);

        {
            let mut selector = self.imp().memo_source_selector.borrow_mut();
            selector.update_sources(items);
            if let Some(settings) = AppSettings::try_new() {
                selector.load_from_settings(&settings);
            }
            *self.imp().hidden_memo_sources.borrow_mut() =
                selector.hidden_uids().into_iter().collect();
        }

        self.render_memo_sidebar();
    }

    pub(crate) fn reload_memo_sources(&self) {
        let page_weak = self.downgrade();
        let (source_sender, source_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let discovery =
                ECalService::new().discover_source_capabilities(ECalComponentType::Memo);
            if let Err(error) = source_sender.send_blocking(discovery) {
                tracing::debug!(%error, "Memo source discovery was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let Ok(discovery) = source_receiver.recv().await else {
                return;
            };
            let (sources, warning) = match discovery {
                Ok(discovery) => (
                    project_memo_source_capabilities(discovery.sources),
                    discovery.warning,
                ),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
            if let Some(warning) = warning {
                tracing::warn!(%warning, "Could not reach EDS memo lists");
            }

            let Some(page) = page_weak.upgrade() else {
                return;
            };
            page.apply_memo_sources(sources);
        });
    }

    pub(crate) fn select_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let Some(memo_list) = self.imp().memo_list.borrow().as_ref().cloned() else {
            return false;
        };
        select_memo_row(&memo_list, source_uid, uid)
    }

    pub fn undo(&self) {
        self.apply_history_action(true);
    }

    pub fn redo(&self) {
        self.apply_history_action(false);
    }

    fn apply_history_action(&self, is_undo: bool) {
        let (Some(memo_list), Some(memo_records), Some(empty_state)) = (
            self.imp().memo_list.borrow().as_ref().cloned(),
            self.imp().memo_records.borrow().as_ref().cloned(),
            self.imp().memo_empty_state.borrow().as_ref().cloned(),
        ) else {
            return;
        };

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let service = ECalService::new();
            let result = if is_undo {
                service.undo(ECalComponentType::Memo)
            } else {
                service.redo(ECalComponentType::Memo)
            };
            let _ = sender.send_blocking(result);
        });

        let page_weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            match result {
                Ok(0) => {
                    let message = if is_undo {
                        crate::i18n::gettext("No memo action to undo")
                    } else {
                        crate::i18n::gettext("No memo action to redo")
                    };
                    page.show_toast(&message);
                }
                Ok(_count) => {
                    reload_memo_snapshot_async(&page, &memo_list, &memo_records, &empty_state);
                    let message = if is_undo {
                        crate::i18n::gettext("Undid last memo action")
                    } else {
                        crate::i18n::gettext("Redid last memo action")
                    };
                    page.show_toast(&message);
                }
                Err(error) => {
                    reload_memo_snapshot_async(&page, &memo_list, &memo_records, &empty_state);
                    let message = if is_undo {
                        crate::i18n::gettext("Undo not possible: {error}")
                            .replace("{error}", &error.to_string())
                    } else {
                        crate::i18n::gettext("Redo not possible: {error}")
                            .replace("{error}", &error.to_string())
                    };
                    page.show_toast(&message);
                }
            }
        });
    }

    /// Rebuild memo rows so category colors and the category filter reflect
    /// the presentation-only category store immediately.
    pub fn refresh_category_presentation(&self) {
        let (Some(memo_list), Some(memo_records), Some(empty_state)) = (
            self.imp().memo_list.borrow().as_ref().cloned(),
            self.imp().memo_records.borrow().as_ref().cloned(),
            self.imp().memo_empty_state.borrow().as_ref().cloned(),
        ) else {
            return;
        };

        let selection = self.imp().memo_selection.borrow().clone();
        while let Some(child) = memo_list.first_child() {
            memo_list.remove(&child);
        }
        for memo in memo_records.borrow().iter() {
            append_memo_row(&memo_list, memo);
        }

        refresh_memo_category_model(self, &memo_list);
        let generation = self.imp().memo_refresh_generation.get().wrapping_add(1);
        self.imp().memo_refresh_generation.set(generation);
        *self.imp().memo_selection.borrow_mut() = selection;
        self.imp()
            .memo_selection
            .borrow_mut()
            .restore_after_refresh(&memo_records.borrow(), generation);
        let restored_selection = self.imp().memo_selection.borrow().clone();
        restore_memo_selection_rows(&memo_list, &restored_selection);
        refresh_memo_filter_view(self, &memo_list, &memo_records, &empty_state);
    }

    fn build_ui(&self) {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.imp().outer_paned.replace(Some(paned.clone()));

        let settings = AppSettings::try_new();
        let initial_memo_search_mode = settings
            .as_ref()
            .map(|settings| MemoSearchMode::from_setting(&settings.memo_search_mode()))
            .unwrap_or_default();
        let initial_memo_category_filter = settings
            .as_ref()
            .map(|settings| settings.memo_category_filter())
            .unwrap_or_else(|| "any".to_string());
        let initial_memo_preview_state = MemoPreviewState {
            visible: settings
                .as_ref()
                .map(AppSettings::memo_preview_visible)
                .unwrap_or(true),
            orientation: settings
                .as_ref()
                .map(|settings| {
                    MemoPreviewOrientation::from_setting(&settings.memo_preview_orientation())
                })
                .unwrap_or_default(),
        };
        self.imp().memo_search_mode.set(initial_memo_search_mode);
        self.imp()
            .memo_category_filter
            .replace(category_filter_option(&initial_memo_category_filter));
        self.imp()
            .memo_preview_state
            .replace(initial_memo_preview_state.clone());

        let memo_sort = Rc::new(Cell::new(
            settings
                .as_ref()
                .map(|settings| MemoSort::from_setting(&settings.memo_sort_field()))
                .unwrap_or_default(),
        ));
        let memo_sort_descending = Rc::new(Cell::new(
            settings
                .as_ref()
                .map(|settings| settings.memo_sort_descending())
                .unwrap_or(true),
        ));
        let memo_source_filter = Rc::new(RefCell::new(
            settings
                .as_ref()
                .map(|settings| settings.memo_source_filter())
                .filter(|source_uid| !source_uid.trim().is_empty()),
        ));
        self.imp()
            .memo_source_filter
            .replace(memo_source_filter.borrow().clone());

        // ── Sidebar (Memo List) ─────────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(0)
            .margin_end(0)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["calendar-sidebar"])
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(&crate::i18n::gettext("Search memos…"))
            .build();
        self.imp().search_entry.replace(Some(search_entry.clone()));
        let search_all = crate::i18n::gettext("All Fields");
        let search_summary = crate::i18n::gettext("Summary");
        let search_desc = crate::i18n::gettext("Description");
        let search_adv = crate::i18n::gettext("Advanced");
        let search_mode_model = gtk::StringList::new(&[
            &search_all,
            &search_summary,
            &search_desc,
            &search_adv,
        ]);
        let search_mode_selector =
            gtk::DropDown::new(Some(search_mode_model), None::<&gtk::Expression>);
        search_mode_selector.set_selected(memo_search_mode_index(initial_memo_search_mode));
        search_mode_selector.set_tooltip_text(Some(&crate::i18n::gettext(
            "Memo search field: summary, description, all fields, or summary:value AND description:value",
        )));
        let sort_updated = crate::i18n::gettext("Last Modified");
        let sort_title = crate::i18n::gettext("Title");
        let sort_source = crate::i18n::gettext("Source");
        let sort_model = gtk::StringList::new(&[&sort_updated, &sort_title, &sort_source]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some(&crate::i18n::gettext("Sort memos")));
        sort_selector.set_selected(match memo_sort.get() {
            MemoSort::Updated => 0,
            MemoSort::Title => 1,
            MemoSort::Source => 2,
        });

        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            let tt = if descending {
                crate::i18n::gettext("Sorted descending")
            } else {
                crate::i18n::gettext("Sorted ascending")
            };
            button.set_tooltip_text(Some(&tt));
            let acc = if descending {
                crate::i18n::gettext("Change sort to ascending")
            } else {
                crate::i18n::gettext("Change sort to descending")
            };
            set_accessible_label(
                button,
                &acc,
            );
        };
        set_direction_button(&direction_button, memo_sort_descending.get());
        let all_memo_lists = crate::i18n::gettext("All Memo Lists");
        let source_model = gtk::StringList::new(&[&all_memo_lists]);
        let source_selector =
            gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        source_selector.set_tooltip_text(Some(&crate::i18n::gettext("Select memo list")));
        let all_categories = crate::i18n::gettext("All Categories");
        let uncategorized = crate::i18n::gettext("Uncategorized");
        let category_model = gtk::StringList::new(&[&all_categories, &uncategorized]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some(&crate::i18n::gettext("Select memo category")));
        self.imp()
            .memo_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .memo_category_selector
            .replace(Some(category_selector.clone()));
        set_category_selector_for_filter(&category_selector, &initial_memo_category_filter);

        let memo_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["navigation-sidebar", "calendar-sidebar-list"])
            .build();
        self.imp()
            .memo_source_list
            .replace(Some(memo_source_list.clone()));
        sidebar_box.append(&memo_source_list);

        let memo_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
            .css_classes(vec!["navigation-sidebar", "memo-list"])
            .build();

        let memo_sort_for_list = Rc::clone(&memo_sort);
        let memo_sort_descending_for_list = Rc::clone(&memo_sort_descending);
        memo_list.set_sort_func(move |left, right| {
            compare_memo_rows(
                left,
                right,
                memo_sort_for_list.get(),
                memo_sort_descending_for_list.get(),
            )
            .into()
        });

        let memo_sort_for_selector = Rc::clone(&memo_sort);
        let memo_list_for_sort = memo_list.clone();
        sort_selector.connect_selected_notify(move |selector| {
            let sort = match selector.selected() {
                1 => MemoSort::Title,
                2 => MemoSort::Source,
                _ => MemoSort::Updated,
            };
            memo_sort_for_selector.set(sort);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_sort_field(sort.setting_value());
            }
            memo_list_for_sort.invalidate_sort();
        });

        let memo_sort_descending_for_button = Rc::clone(&memo_sort_descending);
        let memo_list_for_direction = memo_list.clone();
        let direction_button_for_signal = direction_button.clone();
        direction_button.connect_clicked(move |_| {
            let descending = !memo_sort_descending_for_button.get();
            memo_sort_descending_for_button.set(descending);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_sort_descending(descending);
            }
            set_direction_button(&direction_button_for_signal, descending);
            memo_list_for_direction.invalidate_sort();
        });

        let memo_records = Rc::new(RefCell::new(Vec::<MemoRecord>::new()));
        let selected_memo = Rc::new(RefCell::new(None::<MemoRecord>));
        let memo_mutation_generation = Rc::new(Cell::new(0u64));

        let empty_state = gtk::Label::builder()
            .label(&crate::i18n::gettext("No synchronized memos loaded"))
            .css_classes(vec!["dim-label"])
            .wrap(false)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        self.imp().memo_list.replace(Some(memo_list.clone()));
        self.imp()
            .memo_records
            .replace(Some(Rc::clone(&memo_records)));
        self.imp()
            .memo_empty_state
            .replace(Some(empty_state.clone()));
        self.imp()
            .memo_search_query
            .replace(search_entry.text().to_string());

        let page_weak_for_search_mode = self.downgrade();
        let memo_list_for_search_mode = memo_list.clone();
        let memo_records_for_search_mode = Rc::clone(&memo_records);
        let empty_state_for_search_mode = empty_state.clone();
        search_mode_selector.connect_selected_notify(move |selector| {
            let mode = memo_search_mode_from_index(selector.selected());
            if let Some(page) = page_weak_for_search_mode.upgrade() {
                page.imp().memo_search_mode.set(mode);
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_memo_search_mode(mode.setting_value());
                }
                refresh_memo_filter_view(
                    &page,
                    &memo_list_for_search_mode,
                    &memo_records_for_search_mode,
                    &empty_state_for_search_mode,
                );
            }
        });

        let page_weak_for_search = self.downgrade();
        let empty_state_for_search = empty_state.clone();
        let memo_list_for_search = memo_list.clone();
        let memo_records_for_search = Rc::clone(&memo_records);
        search_entry.connect_search_changed(glib::clone!(
            #[strong]
            memo_list_for_search,
            move |entry| {
                // Advanced mode has a deliberately case-sensitive grammar
                // (`AND` and lower-case field names).  Field matching itself
                // remains case-insensitive in MemoFilter, so lower-casing the
                // complete query here only corrupts the grammar.
                let query = entry.text().trim().to_string();
                if let Some(page) = page_weak_for_search.upgrade() {
                    page.imp().memo_search_query.replace(query.clone());
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_search,
                        &memo_records_for_search,
                        &empty_state_for_search,
                    );
                }
            }
        ));


        let page_weak_for_category = self.downgrade();
        let category_model_for_selector = category_model.clone();
        let memo_list_for_category = memo_list.clone();
        let memo_records_for_category = Rc::clone(&memo_records);
        let empty_state_for_category = empty_state.clone();
        category_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_category.upgrade() else {
                return;
            };
            let category = if selector.selected() == 0 {
                None
            } else if selector.selected() == 1 {
                Some("without".to_string())
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
            };
            page.imp().memo_category_filter.replace(category.clone());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_category_filter(category.as_deref().unwrap_or("any"));
            }
            refresh_memo_filter_view(
                &page,
                &memo_list_for_category,
                &memo_records_for_category,
                &empty_state_for_category,
            );
        });

        self.refresh_hidden_memo_sources();
        self.apply_memo_sources(Vec::new());
        self.reload_memo_sources();

        if let Some(settings) = AppSettings::try_new() {
            let memo_sort_for_settings = Rc::clone(&memo_sort);
            let sort_selector_for_settings = sort_selector.clone();
            let memo_list_for_sort_settings = memo_list.clone();
            settings.connect_changed(Some("memo-sort-field"), move |settings, _| {
                let sort = MemoSort::from_setting(settings.string("memo-sort-field").as_str());
                memo_sort_for_settings.set(sort);
                sort_selector_for_settings.set_selected(match sort {
                    MemoSort::Updated => 0,
                    MemoSort::Title => 1,
                    MemoSort::Source => 2,
                });
                memo_list_for_sort_settings.invalidate_sort();
            });

            let memo_sort_descending_for_settings = Rc::clone(&memo_sort_descending);
            let direction_button_for_settings = direction_button.clone();
            let memo_list_for_direction_settings = memo_list.clone();
            settings.connect_changed(Some("memo-sort-descending"), move |settings, _| {
                let descending = settings.boolean("memo-sort-descending");
                memo_sort_descending_for_settings.set(descending);
                direction_button_for_settings.set_icon_name(if descending {
                    "view-sort-descending-symbolic"
                } else {
                    "view-sort-ascending-symbolic"
                });
                let tt = if descending {
                    crate::i18n::gettext("Sorted descending")
                } else {
                    crate::i18n::gettext("Sorted ascending")
                };
                direction_button_for_settings.set_tooltip_text(Some(&tt));
                let acc = if descending {
                    crate::i18n::gettext("Change sort to ascending")
                } else {
                    crate::i18n::gettext("Change sort to descending")
                };
                set_accessible_label(
                    &direction_button_for_settings,
                    &acc,
                );
                memo_list_for_direction_settings.invalidate_sort();
            });

            let page_weak_hidden = self.downgrade();
            settings.connect_changed(Some("memo-hidden-sources"), move |_, _| {
                if let Some(page) = page_weak_hidden.upgrade() {
                    page.refresh_hidden_memo_sources();
                    page.refresh_active_memo_list();
                }
            });

            let page_weak_default = self.downgrade();
            settings.connect_changed(Some("default-memo-source"), move |_, _| {
                if let Some(page) = page_weak_default.upgrade() {
                    if let Some(settings) = AppSettings::try_new() {
                        page.imp().memo_source_selector.borrow_mut().load_from_settings(&settings);
                    }
                    page.render_memo_sidebar();
                }
            });

            let page_weak_groups = self.downgrade();
            settings.connect_changed(Some("memo-groups-order"), move |_, _| {
                if let Some(page) = page_weak_groups.upgrade() {
                    if let Some(settings) = AppSettings::try_new() {
                        page.imp().memo_source_selector.borrow_mut().load_from_settings(&settings);
                    }
                    page.render_memo_sidebar();
                }
            });

            let page_weak_hidden_groups = self.downgrade();
            settings.connect_changed(Some("memo-hidden-groups"), move |_, _| {
                if let Some(page) = page_weak_hidden_groups.upgrade() {
                    if let Some(settings) = AppSettings::try_new() {
                        page.imp().memo_source_selector.borrow_mut().load_from_settings(&settings);
                    }
                    page.render_memo_sidebar();
                }
            });

            let search_mode_selector_for_settings = search_mode_selector.clone();
            let memo_list_for_search_mode_settings = memo_list.clone();
            let memo_records_for_search_mode_settings = Rc::clone(&memo_records);
            let empty_state_for_search_mode_settings = empty_state.clone();
            let page_weak_for_search_mode_settings = self.downgrade();
            settings.connect_changed(Some("memo-search-mode"), move |settings, _| {
                let mode = MemoSearchMode::from_setting(&settings.string("memo-search-mode"));
                search_mode_selector_for_settings.set_selected(memo_search_mode_index(mode));
                if let Some(page) = page_weak_for_search_mode_settings.upgrade() {
                    page.imp().memo_search_mode.set(mode);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_search_mode_settings,
                        &memo_records_for_search_mode_settings,
                        &empty_state_for_search_mode_settings,
                    );
                }
            });

            let category_selector_for_settings = category_selector.clone();
            let memo_list_for_category_settings = memo_list.clone();
            let memo_records_for_category_settings = Rc::clone(&memo_records);
            let empty_state_for_category_settings = empty_state.clone();
            let page_weak_for_category_settings = self.downgrade();
            settings.connect_changed(Some("memo-category-filter"), move |settings, _| {
                let value = settings.string("memo-category-filter").to_string();
                if let Some(page) = page_weak_for_category_settings.upgrade() {
                    page.imp()
                        .memo_category_filter
                        .replace(category_filter_option(&value));
                    refresh_memo_category_model(&page, &memo_list_for_category_settings);
                    set_category_selector_for_filter(&category_selector_for_settings, &value);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_category_settings,
                        &memo_records_for_category_settings,
                        &empty_state_for_category_settings,
                    );
                }
            });
        }


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
        let drawer_icon = gtk::Image::from_icon_name("accessories-text-editor-symbolic");
        let drawer_title = gtk::Label::builder()
            .label(&crate::i18n::gettext("Memos"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(vec!["meridian-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "memos",
            &sidebar_outer_box,
        ));

        paned.set_start_child(Some(&sidebar_outer_box));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(true);

        // ── Main Content (Memo Detail View) ──────────────────────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(12)
            .build();

        let memo_title = gtk::Entry::builder()
            .placeholder_text(&crate::i18n::gettext("No memo selected"))
            .css_classes(vec!["title-1"])
            .build();

        let memo_meta = gtk::Label::builder()
            .label(&crate::i18n::gettext("Memos will be loaded after synchronization"))
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Start)
            .build();

        let memo_text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .margin_top(8)
            .build();
        memo_text
            .buffer()
            .set_text(&crate::i18n::gettext("No memo data available yet."));

        let memo_title_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_bottom(4)
            .build();
        memo_title.set_hexpand(true);
        memo_title_box.append(&memo_title);
        content_box.append(&memo_title_box);

        let save_memo_btn = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(&crate::i18n::gettext("Save memo (Ctrl+S)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&save_memo_btn, &crate::i18n::gettext("Save memo"));
        let delete_memo_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&crate::i18n::gettext("Delete memo (Delete)"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&delete_memo_btn, &crate::i18n::gettext("Delete memo"));
        let export_memo_btn = gtk::Button::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text(&crate::i18n::gettext("Export memo"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&export_memo_btn, &crate::i18n::gettext("Export memo"));

        let open_memo_btn = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text(&crate::i18n::gettext("Open memo"))
            .build();
        let forward_memo_btn = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text(&crate::i18n::gettext("Forward memo"))
            .build();
        let open_url_memo_btn = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text(&crate::i18n::gettext("Open memo URL"))
            .build();
        let print_memo_btn = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text(&crate::i18n::gettext("Print memo"))
            .build();
        let save_as_memo_btn = gtk::Button::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text(&crate::i18n::gettext("Save memo as"))
            .build();
        let copy_memo_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(&crate::i18n::gettext("Copy memo"))
            .build();
        let cut_memo_btn = gtk::Button::builder()
            .icon_name("edit-cut-symbolic")
            .tooltip_text(&crate::i18n::gettext("Cut memo"))
            .build();
        let paste_memo_btn = gtk::Button::builder()
            .icon_name("edit-paste-symbolic")
            .tooltip_text(&crate::i18n::gettext("Paste memo"))
            .build();
        let select_all_memo_btn = gtk::Button::builder()
            .icon_name("edit-select-all-symbolic")
            .tooltip_text(&crate::i18n::gettext("Select all memos"))
            .build();
        let classic_text = crate::i18n::gettext("Classic");
        let vertical_text = crate::i18n::gettext("Vertical");
        let preview_orientation_model = gtk::StringList::new(&[&classic_text, &vertical_text]);
        let preview_orientation_selector =
            gtk::DropDown::new(Some(preview_orientation_model), None::<&gtk::Expression>);
        preview_orientation_selector.set_selected(match initial_memo_preview_state.orientation {
            MemoPreviewOrientation::Classic => 0,
            MemoPreviewOrientation::Vertical => 1,
        });
        preview_orientation_selector.set_tooltip_text(Some(&crate::i18n::gettext("Memo preview orientation")));
        preview_orientation_selector.set_valign(gtk::Align::Center);

        let more_popover = gtk::Popover::builder().build();
        let more_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(4)
            .margin_end(4)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let configure_menu_button =
            |btn: &gtk::Button, icon: &str, label: &str, popover: &gtk::Popover| {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let img = gtk::Image::from_icon_name(icon);
                let lbl = gtk::Label::builder()
                    .label(label)
                    .halign(gtk::Align::Start)
                    .hexpand(true)
                    .build();
                row_box.append(&img);
                row_box.append(&lbl);
                btn.set_child(Some(&row_box));
                btn.add_css_class("flat");
                btn.set_halign(gtk::Align::Fill);
                let p = popover.clone();
                btn.connect_clicked(move |_| {
                    p.popdown();
                });
            };

        configure_menu_button(&open_memo_btn, "document-open-symbolic", &crate::i18n::gettext("Open"), &more_popover);
        configure_menu_button(&forward_memo_btn, "mail-forward-symbolic", &crate::i18n::gettext("Forward"), &more_popover);
        configure_menu_button(&open_url_memo_btn, "external-link-symbolic", &crate::i18n::gettext("Open URL"), &more_popover);
        configure_menu_button(&export_memo_btn, "document-save-as-symbolic", &crate::i18n::gettext("Export…"), &more_popover);
        configure_menu_button(&save_as_memo_btn, "document-save-as-symbolic", &crate::i18n::gettext("Save As…"), &more_popover);
        configure_menu_button(&print_memo_btn, "document-print-symbolic", &crate::i18n::gettext("Print Note… (Ctrl+P)"), &more_popover);
        configure_menu_button(&cut_memo_btn, "edit-cut-symbolic", &crate::i18n::gettext("Cut"), &more_popover);
        configure_menu_button(&copy_memo_btn, "edit-copy-symbolic", &crate::i18n::gettext("Copy"), &more_popover);
        configure_menu_button(&paste_memo_btn, "edit-paste-symbolic", &crate::i18n::gettext("Paste"), &more_popover);
        configure_menu_button(&select_all_memo_btn, "edit-select-all-symbolic", &crate::i18n::gettext("Select All"), &more_popover);

        more_box.append(&open_memo_btn);
        more_box.append(&forward_memo_btn);
        more_box.append(&open_url_memo_btn);
        more_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        more_box.append(&export_memo_btn);
        more_box.append(&save_as_memo_btn);
        more_box.append(&print_memo_btn);
        more_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        more_box.append(&cut_memo_btn);
        more_box.append(&copy_memo_btn);
        more_box.append(&paste_memo_btn);
        more_box.append(&select_all_memo_btn);
        more_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let orient_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        orient_box.set_margin_start(6);
        orient_box.set_margin_end(6);
        orient_box.set_margin_top(4);
        orient_box.set_margin_bottom(4);
        let orient_label = gtk::Label::builder()
            .label(&crate::i18n::gettext("Preview"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        orient_box.append(&orient_label);
        orient_box.append(&preview_orientation_selector);
        more_box.append(&orient_box);

        more_popover.set_child(Some(&more_box));

        let more_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&more_popover)
            .tooltip_text(&crate::i18n::gettext("More Actions"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&more_button, &crate::i18n::gettext("More Actions"));

        self.imp().memo_surface_buttons.replace(Some(vec![
            save_memo_btn.clone(),
            export_memo_btn.clone(),
            delete_memo_btn.clone(),
            open_memo_btn.clone(),
            forward_memo_btn.clone(),
            open_url_memo_btn.clone(),
            print_memo_btn.clone(),
            save_as_memo_btn.clone(),
            copy_memo_btn.clone(),
            cut_memo_btn.clone(),
            paste_memo_btn.clone(),
            select_all_memo_btn.clone(),
        ]));

        self.imp().save_memo_button.replace(Some(save_memo_btn.clone()));
        self.imp().delete_memo_button.replace(Some(delete_memo_btn.clone()));
        self.imp().print_memo_button.replace(Some(print_memo_btn.clone()));

        let memo_search_bar = gtk::SearchBar::builder()
            .search_mode_enabled(false)
            .key_capture_widget(self)
            .build();
        memo_search_bar.connect_entry(&search_entry);
        self.imp().memo_search_bar.replace(Some(memo_search_bar.clone()));

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
        search_entry.set_hexpand(false);
        search_entry.set_valign(gtk::Align::Center);
        search_mode_selector.set_valign(gtk::Align::Center);

        search_content_box.append(&search_entry);
        search_content_box.append(&search_mode_selector);
        memo_search_bar.set_child(Some(&search_content_box));

        let entry_for_search_mode = search_entry.clone();
        memo_search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            if !bar.is_search_mode() && !entry_for_search_mode.text().is_empty() {
                entry_for_search_mode.set_text("");
            }
        });

        let search_bar_for_stop = memo_search_bar.clone();
        search_entry.connect_stop_search(move |_| {
            search_bar_for_stop.set_search_mode(false);
        });

        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle memo preview (Ctrl+M)"))
            .active(initial_memo_preview_state.visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&preview_toggle, &crate::i18n::gettext("Toggle memo preview"));
        self.imp().memo_preview_toggle.replace(Some(preview_toggle.clone()));

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
        self.imp().memo_sidebar_toggle.replace(Some(sidebar_toggle.clone()));

        let btn_add_memo = gtk::Button::builder()
            .icon_name("document-new-symbolic")
            .tooltip_text(&crate::i18n::gettext("Create new note (Ctrl+N)"))
            .css_classes(vec!["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        set_button_icon_with_fallback(&btn_add_memo, "document-new-symbolic", "list-add-symbolic");
        set_accessible_label(&btn_add_memo, &crate::i18n::gettext("New Memo"));
        self.imp().new_memo_button.replace(Some(btn_add_memo.clone()));

        let memo_list_for_new = memo_list.clone();
        let memo_records_for_new = Rc::clone(&memo_records);
        let empty_state_for_new = empty_state.clone();
        let page_weak_for_new = self.downgrade();
        btn_add_memo.connect_clicked(move |_| {
            if let Some(page) = page_weak_for_new.upgrade() {
                page.open_memo_dialog(
                    memo_list_for_new.clone(),
                    Rc::clone(&memo_records_for_new),
                    empty_state_for_new.clone(),
                    None,
                );
            }
        });

        let memos_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .hexpand(true)
            .css_classes(vec!["unified-view-toolbar", "toolbar"])
            .build();

        let left_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        let action_group = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(vec!["linked"])
            .valign(gtk::Align::Center)
            .build();
        action_group.append(&save_memo_btn);
        action_group.append(&delete_memo_btn);
        left_box.append(&action_group);
        left_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        left_box.append(&more_button);

        let spacer = gtk::Box::builder()
            .hexpand(true)
            .build();

        let right_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();

        let sort_group = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .css_classes(vec!["linked"])
            .build();
        sort_selector.set_valign(gtk::Align::Center);
        direction_button.set_valign(gtk::Align::Center);
        sort_group.append(&sort_selector);
        sort_group.append(&direction_button);
        right_box.append(&sort_group);
        right_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        category_selector.set_valign(gtk::Align::Center);
        right_box.append(&category_selector);

        memos_toolbar.append(&left_box);
        memos_toolbar.append(&spacer);
        memos_toolbar.append(&right_box);

        let memos_toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .min_content_width(0)
            .child(&memos_toolbar)
            .build();
        memos_toolbar_scroll.set_visible(
            AppSettings::try_new()
                .map(|settings| settings.memo_show_toolbar())
                .unwrap_or(false),
        );
        self.imp()
            .toolbar
            .replace(Some(memos_toolbar_scroll.clone().upcast()));

        let memo_preview_label = gtk::Label::builder()
            .label(&crate::i18n::gettext("No unique memo selection"))
            .use_markup(true)
            .wrap(true)
            .selectable(true)
            .xalign(0.0)
            .halign(gtk::Align::Fill)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .visible(false)
            .build();
        self.imp()
            .memo_preview_label
            .replace(Some(memo_preview_label.clone()));
        content_box.append(&memo_preview_label);

        let memo_categories = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Categories (comma separated)"))
            .build();
        memo_categories.set_tooltip_text(Some(&crate::i18n::gettext("e.g. Work, Personal")));
        content_box.append(&memo_categories);
        let memo_alarm_editor = AlarmEditor::new(None, "");
        content_box.append(memo_alarm_editor.widget());
        content_box.append(&memo_meta);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content_box.append(&memo_text);

        // Panel 2: Memos List Container with Quick Add
        let memo_list_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .css_classes(vec!["memo-list-pane"])
            .build();

        let quick_add_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .css_classes(vec!["memo-quick-add-box", "task-quick-add-box"])
            .build();
        let quick_add_icon = gtk::Image::builder()
            .icon_name("list-add-symbolic")
            .pixel_size(16)
            .css_classes(vec!["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        let quick_add_entry = gtk::Entry::builder()
            .placeholder_text(&crate::i18n::gettext("Add new note… (press Enter)"))
            .hexpand(true)
            .css_classes(vec!["memo-quick-add-entry", "task-quick-add-entry"])
            .build();
        set_accessible_label(&quick_add_entry, &crate::i18n::gettext("Quickly add new note"));
        quick_add_box.append(&quick_add_icon);
        quick_add_box.append(&quick_add_entry);
        memo_list_container.append(&quick_add_box);
        memo_list_container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let memo_list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .min_content_width(0)
            .build();

        let memo_table_header = gtk::DrawingArea::builder()
            .content_height(28)
            .hexpand(true)
            .css_classes(vec!["memos-table-header"])
            .build();
        set_accessible_label(&memo_table_header, &crate::i18n::gettext("Memo Table Header"));

        let memo_table_canvas = gtk::DrawingArea::builder()
            .content_height(100)
            .hexpand(true)
            .focusable(true)
            .can_focus(true)
            .focus_on_click(true)
            .css_classes(vec!["memos-table-canvas"])
            .build();
        set_accessible_role(&memo_table_canvas, gtk::AccessibleRole::List);
        set_accessible_label(&memo_table_canvas, &crate::i18n::gettext("Memos List"));

        self.imp()
            .memo_table_header
            .replace(Some(memo_table_header.clone()));
        self.imp()
            .memo_table_canvas
            .replace(Some(memo_table_canvas.clone()));

        let memo_list_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        memo_list.set_visible(false);
        memo_list_box.append(&empty_state);
        memo_list_box.append(&memo_table_header);
        memo_list_box.append(&memo_table_canvas);
        memo_list_box.append(&memo_list);
        memo_list_scroll.set_child(Some(&memo_list_box));
        memo_list_container.append(&memo_list_scroll);

        let page_weak_for_quick_add = self.downgrade();
        let memo_records_for_quick_add = Rc::clone(&memo_records);
        let memo_list_for_quick_add = memo_list.clone();
        let empty_state_for_quick_add = empty_state.clone();
        let memo_text_for_quick_add = memo_text.clone();
        quick_add_entry.connect_activate(move |entry| {
            let text = entry.text().trim().to_string();
            if text.is_empty() {
                return;
            }
            let Some(page) = page_weak_for_quick_add.upgrade() else {
                return;
            };

            let preferred = AppSettings::try_new()
                .map(|settings| settings.default_memo_source_uid())
                .filter(|uid| !uid.trim().is_empty());

            let target_source = if let Some(filtered) = page.imp().memo_source_filter.borrow().clone() {
                if memo_source_is_writable_for_page(&page, &filtered) {
                    filtered
                } else {
                    preferred.unwrap_or_else(|| "local-calendar".to_string())
                }
            } else {
                preferred.unwrap_or_else(|| "local-calendar".to_string())
            };

            let component = match build_memo_component(&text, "", "", &[]) {
                Ok(c) => c,
                Err(err) => {
                    show_toast_for_widget(entry, &err);
                    return;
                }
            };

            entry.set_text("");
            entry.set_sensitive(false);

            let entry_for_done = entry.clone();
            let page_for_done = page.downgrade();
            let text_for_toast = text.clone();
            let target_source_for_thread = target_source.clone();
            let target_source_for_save = target_source.clone();
            let memo_list_for_done = memo_list_for_quick_add.clone();
            let memo_records_for_done = Rc::clone(&memo_records_for_quick_add);
            let empty_state_for_done = empty_state_for_quick_add.clone();
            let memo_text_for_done = memo_text_for_quick_add.clone();
            let (tx, rx) = async_channel::bounded(1);

            std::thread::spawn(move || {
                let res = ECalService::new().create_component(
                    &target_source_for_thread,
                    ECalComponentType::Memo,
                    &component,
                );
                let _ = tx.send_blocking(res);
            });

            glib::spawn_future_local(async move {
                entry_for_done.set_sensitive(true);
                match rx.recv().await {
                    Ok(Ok(result)) => {
                        if let Some(page) = page_for_done.upgrade() {
                            let created_component = match confirmed_memo_component(result.component) {
                                Ok((c, _)) => c,
                                Err(_) => {
                                    build_memo_component(&text_for_toast, "", "", &[]).unwrap_or_default()
                                }
                            };
                            let uid = result.uid;
                            let new_memo = MemoRecord {
                                uid: uid.clone(),
                                source_uid: target_source_for_save.clone(),
                                source_display_name: target_source_for_save.clone(),
                                component: created_component,
                                summary: text_for_toast.clone(),
                                description: String::new(),
                                updated: Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
                            };
                            upsert_memo_row(
                                &memo_list_for_done,
                                Rc::clone(&memo_records_for_done),
                                new_memo,
                            );
                            empty_state_for_done.set_visible(false);
                            refresh_memo_category_model(&page, &memo_list_for_done);
                            refresh_memo_filter_view(
                                &page,
                                &memo_list_for_done,
                                &memo_records_for_done,
                                &empty_state_for_done,
                            );
                            select_memo_row(&memo_list_for_done, &target_source_for_save, &uid);
                            memo_text_for_done.grab_focus();
                            show_toast_for_widget(
                                &entry_for_done,
                                &crate::i18n::gettext("Memo »{}« added").replace("{}", &text_for_toast),
                            );
                        }
                    }
                    Ok(Err(err)) => {
                        show_toast_for_widget(
                            &entry_for_done,
                            &crate::i18n::gettext("Error creating memo: {}").replace("{}", &err.to_string()),
                        );
                        entry_for_done.grab_focus();
                    }
                    Err(_) => {
                        entry_for_done.grab_focus();
                    }
                }
            });
        });

        // Panel 3: Memo Preview / Detail View
        let memo_preview_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(vec!["memo-preview-pane"])
            .build();
        memo_preview_box.set_size_request(220, -1);
        let preview_box = memo_preview_box.clone();

        let memo_detail_stack = gtk::Stack::builder()
            .vexpand(true)
            .hexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();

        let memo_empty_page = adw::StatusPage::builder()
            .icon_name("accessories-text-editor-symbolic")
            .title(&crate::i18n::gettext("No memo selected"))
            .description(&crate::i18n::gettext("Select a memo from the list or create a new one."))
            .vexpand(true)
            .hexpand(true)
            .build();
        memo_detail_stack.add_named(&memo_empty_page, Some("empty"));

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .min_content_width(0)
            .vexpand(true)
            .hexpand(true)
            .child(&content_box)
            .build();
        memo_detail_stack.add_named(&content_scroll, Some("content"));
        memo_detail_stack.set_visible_child_name("empty");
        self.imp()
            .memo_detail_stack
            .replace(Some(memo_detail_stack.clone()));

        preview_box.append(&memo_detail_stack);
        preview_box.set_visible(initial_memo_preview_state.visible);
        self.imp()
            .memo_preview_box
            .replace(Some(preview_box.clone()));

        // Inner Paned: Separates Memo List and Preview/Editor
        memo_list_container.set_size_request(240, -1);
        let memo_preview_paned = gtk::Paned::builder()
            .orientation(memo_paned_orientation(initial_memo_preview_state.orientation))
            .wide_handle(false)
            .vexpand(true)
            .hexpand(true)
            .build();
        memo_preview_paned.set_start_child(Some(&memo_list_container));
        memo_preview_paned.set_end_child(Some(&preview_box));
        memo_preview_paned.set_resize_start_child(true);
        memo_preview_paned.set_resize_end_child(true);
        memo_preview_paned.set_shrink_start_child(false);
        memo_preview_paned.set_shrink_end_child(false);

        let initial_preview_pos = AppSettings::try_new()
            .map(|settings| {
                settings.memo_preview_position(initial_memo_preview_state.orientation.setting_value())
            })
            .unwrap_or_else(|| initial_memo_preview_state.orientation.default_position());
        memo_preview_paned.set_position(initial_preview_pos);
        self.imp()
            .memo_preview_paned
            .replace(Some(memo_preview_paned.clone()));

        let page_weak_for_paned_pos = self.downgrade();
        let clamping_memo_paned = Rc::new(Cell::new(false));
        memo_preview_paned.connect_position_notify(move |paned| {
            if clamping_memo_paned.get() {
                return;
            }
            let Some(page) = page_weak_for_paned_pos.upgrade() else {
                return;
            };
            if page.imp().memo_preview_position_restoring.get() {
                return;
            }
            let both_visible = paned.start_child().is_some_and(|c| c.is_visible())
                && paned.end_child().is_some_and(|c| c.is_visible());
            if !both_visible {
                return;
            }
            let mut pos = paned.position();
            if paned.orientation() == gtk::Orientation::Horizontal {
                let total_w = paned.width();
                if total_w > 480 {
                    let min_pos = (total_w - 500).max(240).min(total_w - 220);
                    let max_pos = (total_w - 220).max(min_pos);
                    let clamped = pos.clamp(min_pos, max_pos);
                    if clamped != pos {
                        clamping_memo_paned.set(true);
                        paned.set_position(clamped);
                        clamping_memo_paned.set(false);
                        pos = clamped;
                    }
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_memo_preview_position(
                            MemoPreviewOrientation::Vertical.setting_value(),
                            pos,
                        );
                    }
                }
            } else {
                let total_h = paned.height();
                if total_h > 340 {
                    let min_pos = 160;
                    let max_pos = (total_h - 160).max(min_pos);
                    let clamped = pos.clamp(min_pos, max_pos);
                    if clamped != pos {
                        clamping_memo_paned.set(true);
                        paned.set_position(clamped);
                        clamping_memo_paned.set(false);
                        pos = clamped;
                    }
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_memo_preview_position(
                            MemoPreviewOrientation::Classic.setting_value(),
                            pos,
                        );
                    }
                }
            }
        });

        if let Some(settings) = AppSettings::try_new() {
            let preview_toggle_for_settings = preview_toggle.clone();
            let preview_box_for_settings = preview_box.clone();
            let page_weak_for_preview_settings = self.downgrade();
            let memo_list_for_preview_settings = memo_list.clone();
            let memo_records_for_preview_settings = Rc::clone(&memo_records);
            settings.connect_changed(Some("memo-preview-visible"), move |settings, _| {
                let visible = settings.boolean("memo-preview-visible");
                if preview_toggle_for_settings.is_active() != visible {
                    preview_toggle_for_settings.set_active(visible);
                }
                preview_box_for_settings.set_visible(visible);
                if let Some(page) = page_weak_for_preview_settings.upgrade() {
                    page.imp()
                        .memo_preview_state
                        .borrow_mut()
                        .set_visible(visible);
                    refresh_memo_preview(
                        &page,
                        &memo_list_for_preview_settings,
                        &memo_records_for_preview_settings,
                    );
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_right_sidebar_toggle();
                    }
                }
            });

            let preview_orientation_selector_for_settings = preview_orientation_selector.clone();
            let page_weak_for_orientation_settings = self.downgrade();
            let memo_list_for_orientation_settings = memo_list.clone();
            let memo_records_for_orientation_settings = Rc::clone(&memo_records);
            settings.connect_changed(Some("memo-preview-orientation"), move |settings, _| {
                let orientation = MemoPreviewOrientation::from_setting(
                    &settings.string("memo-preview-orientation"),
                );
                preview_orientation_selector_for_settings.set_selected(orientation.index());
                if let Some(page) = page_weak_for_orientation_settings.upgrade() {
                    page.imp()
                        .memo_preview_state
                        .borrow_mut()
                        .set_orientation(orientation);
                    apply_memo_preview_paned_layout(&page, orientation);
                    refresh_memo_preview(
                        &page,
                        &memo_list_for_orientation_settings,
                        &memo_records_for_orientation_settings,
                    );
                }
            });

            let page_weak_for_toolbar = self.downgrade();
            settings.connect_changed(Some("memo-show-toolbar"), move |settings, _| {
                if let Some(page) = page_weak_for_toolbar.upgrade() {
                    if !page.imp().is_mobile.get() {
                        if let Some(toolbar) = page.imp().toolbar.borrow().as_ref() {
                            toolbar.set_visible(settings.boolean("memo-show-toolbar"));
                        }
                    }
                }
            });
        }

        let save_memo_btn_for_context = save_memo_btn.clone();
        let export_memo_btn_for_context = export_memo_btn.clone();
        let delete_memo_btn_for_context = delete_memo_btn.clone();
        let memo_records_for_save = Rc::clone(&memo_records);
        let page_weak_for_save = self.downgrade();
        let memo_title_for_save = memo_title.clone();
        let memo_categories_for_save = memo_categories.clone();
        let memo_text_for_save = memo_text.clone();
        let memo_alarm_editor_for_save = memo_alarm_editor.clone();
        let memo_list_for_save = memo_list.clone();
        let memo_meta_for_save = memo_meta.clone();
        save_memo_btn.connect_clicked(move |_| {
            let Some(page) = page_weak_for_save.upgrade() else {
                return;
            };
            let mut selected =
                match selected_memo_records_checked(&memo_list_for_save, &memo_records_for_save) {
                    Ok(selected) if selected.len() == 1 => selected,
                    Ok(_) => {
                        show_toast_for_widget(
                            &memo_title_for_save,
                            &crate::i18n::gettext("Saving is only available for exactly one memo"),
                        );
                        return;
                    }
                    Err(error) => {
                        show_toast_for_widget(&memo_title_for_save, &error);
                        return;
                    }
                };
            let Some(memo) = selected.pop() else {
                show_toast_for_widget(&memo_title_for_save, &crate::i18n::gettext("No memo selected"));
                return;
            };
            if !memo_source_is_writable_for_page(&page, &memo.source_uid) {
                show_toast_for_widget(
                    &memo_title_for_save,
                    &crate::i18n::gettext("This memo is from a read-only imported source"),
                );
                return;
            }
            let title = memo_title_for_save.text().to_string();
            let description = memo_text_for_save
                .buffer()
                .text(
                    &memo_text_for_save.buffer().start_iter(),
                    &memo_text_for_save.buffer().end_iter(),
                    false,
                )
                .to_string();
            let mut draft = match MemoEditorDraft::parse(&memo.component) {
                Ok(draft) => draft,
                Err(error) => {
                    show_toast_for_widget(&memo_title_for_save, &error);
                    return;
                }
            };
            draft.summary = title.clone();
            draft.description = description.clone();
            draft.categories = parse_categories_input(&memo_categories_for_save.text());
            memo_alarm_editor_for_save.set_summary(&title);
            draft.alarms = memo_alarm_editor_for_save.alarms();
            let component = match draft.apply() {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&memo_title_for_save, &error);
                    return;
                }
            };
            let modified = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let component = crate::services::calendar_store_service::update_ical_property(
                &component,
                "LAST-MODIFIED",
                Some(&modified),
            );

            crate::services::ecal_service::register_known_ecal_component(
                ECalComponentType::Memo,
                &memo.source_uid,
                &memo.uid,
                &memo.component,
            );
            let (tx, rx) = async_channel::bounded(1);
            let source_uid = memo.source_uid.clone();
            let memo_uid = memo.uid.clone();
            let component_for_save = component.clone();
            std::thread::spawn(move || {
                let res = ECalService::new().modify_component(
                    &source_uid,
                    ECalComponentType::Memo,
                    &memo_uid,
                    &component_for_save,
                );
                let _ = tx.send_blocking(res);
            });

            let page_weak_for_save_done = page.downgrade();
            let memo_title_for_toast = memo_title_for_save.clone();
            let memo_list_for_save_done = memo_list_for_save.clone();
            let memo_records_for_save_done = Rc::clone(&memo_records_for_save);
            let memo_meta_for_save_done = memo_meta_for_save.clone();
            glib::spawn_future_local(async move {
                match rx.recv().await {
                    Ok(Ok(result)) => {
                        let final_comp = result.component.unwrap_or(component);
                        let updated_memo = MemoRecord {
                            uid: memo.uid.clone(),
                            source_uid: memo.source_uid.clone(),
                            source_display_name: memo.source_display_name.clone(),
                            component: final_comp,
                            summary: title,
                            description,
                            updated: modified.clone(),
                        };
                        upsert_memo_row(
                            &memo_list_for_save_done,
                            Rc::clone(&memo_records_for_save_done),
                            updated_memo,
                        );
                        memo_meta_for_save_done.set_label(&format!("VJOURNAL · {modified}"));
                        if let Some(page) = page_weak_for_save_done.upgrade() {
                            refresh_memo_category_model(&page, &memo_list_for_save_done);
                        }
                        show_toast_for_widget(&memo_title_for_toast, &crate::i18n::gettext("Memo saved"));
                    }
                    Ok(Err(err)) => {
                        show_toast_for_widget(
                            &memo_title_for_toast,
                            &crate::i18n::gettext("Error saving: {}").replace("{}", &err.to_string()),
                        );
                    }
                    Err(_) => {}
                }
            });
        });

        let memo_list_for_export = memo_list.clone();
        let memo_records_for_export = Rc::clone(&memo_records);
        export_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(
                &memo_list_for_export,
                &memo_records_for_export,
            ) {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_multi_export(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title(&crate::i18n::gettext("Save memo as iCalendar"))
                .initial_name("memo.ics")
                .modal(true)
                .build();
            let button_for_result = button.clone();
            file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else {
                    return;
                };
                save_memo_file_async(
                    &button_for_result,
                    file,
                    payload,
                    crate::i18n::gettext("Memo(s) exported as iCalendar"),
                );
            });
        });

        let memo_list_for_open = memo_list.clone();
        let records_for_open = Rc::clone(&memo_records);
        let empty_state_for_open = empty_state.clone();
        let page_weak_for_open = self.downgrade();
        open_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_open, &records_for_open) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(button, &crate::i18n::gettext("Opening is only available for exactly one memo"));
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            let Some(page) = page_weak_for_open.upgrade() else {
                return;
            };
            page.open_memo_dialog(
                memo_list_for_open.clone(),
                Rc::clone(&records_for_open),
                empty_state_for_open.clone(),
                Some(memo),
            );
        });

        let memo_list_for_forward = memo_list.clone();
        let records_for_forward = Rc::clone(&memo_records);
        forward_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_forward, &records_for_forward) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(button, &crate::i18n::gettext("Forwarding is only available for exactly one memo"));
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            match build_forward_publish_payload(&memo) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(
                        button,
                        &crate::i18n::gettext("METHOD:PUBLISH provided; real iTIP forwarding is Linux handoff"),
                    );
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_url = memo_list.clone();
        let records_for_url = Rc::clone(&memo_records);
        open_url_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(&memo_list_for_url, &records_for_url)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            if selected.len() != 1 {
                show_toast_for_widget(button, &crate::i18n::gettext("Open URL is only available for exactly one memo"));
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            match validated_url_for_record(&memo) {
                Ok(url) => {
                    let button_for_result = button.clone();
                    gio::AppInfo::launch_default_for_uri_async(
                        &url,
                        None::<&gio::AppLaunchContext>,
                        None::<&gio::Cancellable>,
                        move |result| match result {
                            Ok(()) => show_toast_for_widget(&button_for_result, &crate::i18n::gettext("URL opened")),
                            Err(error) => show_toast_for_widget(
                                &button_for_result,
                                &crate::i18n::gettext("Could not open URL: {}").replace("{}", &error.to_string()),
                            ),
                        },
                    );
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_print = memo_list.clone();
        let records_for_print = Rc::clone(&memo_records);
        print_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(&memo_list_for_print, &records_for_print)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            if selected.len() != 1 {
                show_toast_for_widget(button, &crate::i18n::gettext("Printing is only available for exactly one memo"));
                return;
            }
            let memo = selected.into_iter().next().expect("selection length checked");
            match build_print_document(&memo) {
                Ok(document) => show_toast_for_widget(
                    button,
                    &crate::i18n::gettext("Portable print document prepared ({} characters); Print portal is Linux handoff")
                        .replace("{}", &document.html.len().to_string()),
                ),
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_save_as = memo_list.clone();
        let records_for_save_as = Rc::clone(&memo_records);
        save_as_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_save_as, &records_for_save_as) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(
                    button,
                    &crate::i18n::gettext("Save as is only available for exactly one memo"),
                );
                return;
            }
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_multi_export(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title(&crate::i18n::gettext("Save memo as iCalendar"))
                .initial_name("memos.ics")
                .modal(true)
                .build();
            let button_for_result = button.clone();
            file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else {
                    return;
                };
                save_memo_file_async(
                    &button_for_result,
                    file,
                    payload,
                    crate::i18n::gettext("Memo(s) saved"),
                );
            });
        });

        let memo_list_for_copy = memo_list.clone();
        let records_for_copy = Rc::clone(&memo_records);
        copy_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_copy, &records_for_copy) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            match build_clipboard_payload(&selected, &identities) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(button, &crate::i18n::gettext("Memo(s) copied to clipboard"));
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let cut_payload = Rc::new(RefCell::new(None::<String>));
        let copy_for_cut = copy_memo_btn.clone();
        let delete_for_cut = delete_memo_btn.clone();
        let memo_list_for_cut = memo_list.clone();
        let records_for_cut = Rc::clone(&memo_records);
        let cut_payload_for_cut = Rc::clone(&cut_payload);
        cut_memo_btn.connect_clicked(move |button| {
            if !copy_for_cut.is_sensitive() || !delete_for_cut.is_sensitive() {
                show_toast_for_widget(button, &crate::i18n::gettext("Cut is not available for this selection"));
                return;
            }
            let selected = match selected_memo_records_checked(&memo_list_for_cut, &records_for_cut)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_clipboard_payload(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            cut_payload_for_cut.replace(Some(payload));
            delete_for_cut.emit_clicked();
        });

        let memo_list_for_select_all = memo_list.clone();
        let page_weak_for_select_all = self.downgrade();
        let memo_records_for_select_all = Rc::clone(&memo_records);
        select_all_memo_btn.connect_clicked(move |_| {
            memo_list_for_select_all.select_all();
            let Some(page) = page_weak_for_select_all.upgrade() else { return; };
            let layout_ref = page.imp().memo_table_layout.borrow();
            if let Some(layout) = layout_ref.as_ref() {
                let all_ids: Vec<MemoIdentity> = layout.rows.iter().map(|r| r.identity.clone()).collect();
                let mut sel = MemoSelection::default();
                sel.set_selected(all_ids.clone());
                page.imp().memo_selection.replace(sel);
                unsafe {
                    memo_list_for_select_all.set_data("canvas-selection", all_ids);
                }
                sync_memo_surface_selection(&page, &memo_list_for_select_all);
                update_memo_surface_widgets_from_page(&page, &memo_list_for_select_all, &memo_records_for_select_all);
                refresh_memo_preview(&page, &memo_list_for_select_all, &memo_records_for_select_all);
                if let Some(canvas) = page.imp().memo_table_canvas.borrow().as_ref() {
                    canvas.queue_draw();
                }
            }
        });

        let page_weak_for_paste = self.downgrade();
        let memo_list_for_paste = memo_list.clone();
        let records_for_paste = Rc::clone(&memo_records);
        let empty_state_for_paste = empty_state.clone();
        paste_memo_btn.connect_clicked(move |button| {
            let Some(page) = page_weak_for_paste.upgrade() else {
                return;
            };
            let destination = page
                .imp()
                .memo_source_filter
                .borrow()
                .clone()
                .unwrap_or_else(|| "local-calendar".to_string());
            if !memo_source_is_writable_for_page(&page, &destination) {
                show_toast_for_widget(button, &crate::i18n::gettext("No writable destination source selected"));
                return;
            }
            let clipboard = button.display().clipboard();
            let button_for_result = button.clone();
            let page_weak_for_result = page_weak_for_paste.clone();
            let page_weak_for_clipboard = page_weak_for_paste.clone();
            let memo_list_for_result = memo_list_for_paste.clone();
            let records_for_result = Rc::clone(&records_for_paste);
            let records_for_clipboard = Rc::clone(&records_for_paste);
            let empty_state_for_result = empty_state_for_paste.clone();
            clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
                let payload = match result {
                    Ok(Some(payload)) => payload.to_string(),
                    Ok(None) => {
                        show_toast_for_widget(
                            &button_for_result,
                            &crate::i18n::gettext("The clipboard contains no text"),
                        );
                        return;
                    }
                    Err(error) => {
                        show_toast_for_widget(
                            &button_for_result,
                            &crate::i18n::gettext("Clipboard not readable: {}").replace("{}", &error.to_string()),
                        );
                        return;
                    }
                };
                let Some(page) = page_weak_for_clipboard.upgrade() else {
                    return;
                };
                let current_destination = page
                    .imp()
                    .memo_source_filter
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "local-calendar".to_string());
                if current_destination != destination
                    || !memo_source_is_writable_for_page(&page, &destination)
                {
                    show_toast_for_widget(
                        &button_for_result,
                        &crate::i18n::gettext("The target source has changed or is no longer writable"),
                    );
                    return;
                }
                let existing = records_for_clipboard.borrow().clone();
                let plan = match plan_paste_into(&payload, &destination, true, &existing) {
                    Ok(plan) => plan,
                    Err(error) => {
                        show_toast_for_widget(&button_for_result, &error);
                        return;
                    }
                };
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let result = execute_memo_paste_batch(plan);
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Memo paste result was dropped");
                    }
                });
                glib::spawn_future_local(async move {
                    let Ok(result) = receiver.recv().await else {
                        return;
                    };
                    match result {
                        Ok(successes) => {
                            for (_, memo) in successes {
                                upsert_memo_row(
                                    &memo_list_for_result,
                                    Rc::clone(&records_for_result),
                                    memo,
                                );
                            }
                            if let Some(page) = page_weak_for_result.upgrade() {
                                let generation =
                                    page.imp().memo_refresh_generation.get().wrapping_add(1);
                                page.imp().memo_refresh_generation.set(generation);
                                refresh_memo_category_model(&page, &memo_list_for_result);
                                refresh_memo_filter_view(
                                    &page,
                                    &memo_list_for_result,
                                    &records_for_result,
                                    &empty_state_for_result,
                                );
                            }
                            show_toast_for_widget(&button_for_result, &crate::i18n::gettext("Memo(s) pasted"));
                        }
                        Err(error) => {
                            if let Some(page) = page_weak_for_result.upgrade() {
                                reload_memo_snapshot_async(
                                    &page,
                                    &memo_list_for_result,
                                    &records_for_result,
                                    &empty_state_for_result,
                                );
                            }
                            show_toast_for_widget(&button_for_result, &error);
                        }
                    }
                });
            });
        });

        let memo_records_for_delete = Rc::clone(&memo_records);
        let page_weak_for_delete = self.downgrade();
        let memo_list_for_delete = memo_list.clone();
        let empty_state_for_delete = empty_state.clone();
        let memo_title_for_delete = memo_title.clone();
        let memo_mutation_generation_for_delete = Rc::clone(&memo_mutation_generation);
        let cut_payload_for_delete = Rc::clone(&cut_payload);
        delete_memo_btn.connect_clicked(move |button| {
            let pending_cut_payload = cut_payload_for_delete.borrow_mut().take();
            let Some(page) = page_weak_for_delete.upgrade() else {
                return;
            };
            let selected = match selected_memo_records_checked(
                &memo_list_for_delete,
                &memo_records_for_delete,
            ) {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let planned = match plan_delete(&selected, &identities, |source_uid| {
                memo_source_is_writable_for_page(&page, source_uid)
            }) {
                Ok(planned) => planned,
                Err(result) => {
                    show_toast_for_widget(button, &result.visible_error_text());
                    return;
                }
            };
            if planned.is_empty() {
                show_toast_for_widget(button, &crate::i18n::gettext("No memo selected"));
                return;
            }
            let confirmation_message = if planned.len() == 1 {
                crate::i18n::gettext("The selected memo will be permanently deleted.")
            } else {
                crate::i18n::gettext("The selected memos will be permanently deleted.")
            };
            let delete_generation = memo_mutation_generation_for_delete.get();
            let delete_refresh_generation = page.imp().memo_refresh_generation.get();
            let page_weak_for_action = page_weak_for_delete.clone();
            let page_weak_for_guard = page_weak_for_action.clone();
            let memo_list_for_action = memo_list_for_delete.clone();
            let memo_records_for_action = Rc::clone(&memo_records_for_delete);
            let empty_state_for_action = empty_state_for_delete.clone();
            let memo_mutation_generation_for_action =
                Rc::clone(&memo_mutation_generation_for_delete);
            let button_for_action = button.clone();
            crate::dialogs::run_destructive_action(
                &memo_title_for_delete,
                &crate::i18n::gettext("Delete memos?"),
                &confirmation_message,
                &crate::i18n::gettext("Delete"),
                move || {
                    if memo_mutation_generation_for_action.get() != delete_generation {
                        show_toast_for_widget(
                            &button_for_action,
                            &crate::i18n::gettext("The memo selection is now obsolete"),
                        );
                        return;
                    }
                    let Some(page) = page_weak_for_guard.upgrade() else {
                        return;
                    };
                    if page.imp().memo_refresh_generation.get() != delete_refresh_generation {
                        show_toast_for_widget(
                            &button_for_action,
                            &crate::i18n::gettext("Memo data has been updated in the meantime"),
                        );
                        return;
                    }
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = execute_memo_delete_batch(planned);
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Memo deletion batch result was dropped");
                        }
                    });
                    let page_weak_for_result = page_weak_for_action.clone();
                    let memo_list_for_result = memo_list_for_action.clone();
                    let memo_records_for_result = Rc::clone(&memo_records_for_action);
                    let empty_state_for_result = empty_state_for_action.clone();
                    let button_for_result = button_for_action.clone();
                    glib::spawn_future_local(async move {
                        let Ok(result) = receiver.recv().await else {
                            return;
                        };
                        match result {
                            Ok(identities) => {
                                for identity in &identities {
                                    remove_memo_row(
                                        &memo_list_for_result,
                                        Rc::clone(&memo_records_for_result),
                                        &identity.source_uid,
                                        &identity.uid,
                                    );
                                }
                                if let Some(payload) = pending_cut_payload {
                                    button_for_result.display().clipboard().set_text(&payload);
                                }
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    let generation =
                                        page.imp().memo_refresh_generation.get().wrapping_add(1);
                                    page.imp().memo_refresh_generation.set(generation);
                                    refresh_memo_category_model(&page, &memo_list_for_result);
                                    refresh_memo_filter_view(
                                        &page,
                                        &memo_list_for_result,
                                        &memo_records_for_result,
                                        &empty_state_for_result,
                                    );
                                    if let Some(window) =
                                        page.root().and_downcast::<crate::window::MeridianWindow>()
                                    {
                                        if let Some(view_stack) =
                                            window.imp().view_stack.borrow().as_ref()
                                        {
                                            if let Some(cal_page) = view_stack
                                                .child_by_name("calendar")
                                                .and_downcast::<crate::views::CalendarPage>()
                                            {
                                                for identity in &identities {
                                                    cal_page.remove_memo_from_cache(
                                                        &identity.source_uid,
                                                        &identity.uid,
                                                    );
                                                }
                                            }
                                            if let Some(mail_page) = view_stack
                                                .child_by_name("mail")
                                                .and_downcast::<crate::views::MailPage>()
                                            {
                                                for identity in &identities {
                                                    mail_page.remove_memo_from_cache(
                                                        &identity.source_uid,
                                                        &identity.uid,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                show_toast_for_widget(&button_for_result, &crate::i18n::gettext("Memo(s) deleted"));
                            }
                            Err(error) => {
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    reload_memo_snapshot_async(
                                        &page,
                                        &memo_list_for_result,
                                        &memo_records_for_result,
                                        &empty_state_for_result,
                                    );
                                }
                                show_toast_for_widget(&button_for_result, &error);
                            }
                        }
                    });
                },
            );
        });

        // The original Evolution memo view exposes row actions through a
        // context menu as well as the detail toolbar.  Select the row under
        // the pointer first, then route the menu actions through the existing
        // save/export/delete buttons so all validation and ECal mutation code
        // remains centralized.
        let context_click = gtk::GestureClick::builder().button(3).build();
        let memo_list_weak_for_context = memo_list.downgrade();
        context_click.connect_pressed(move |_, _, x, y| {
            let Some(memo_list_for_context) = memo_list_weak_for_context.upgrade() else {
                return;
            };
            let Some(row) = memo_list_for_context.row_at_y(y as i32) else {
                return;
            };
            memo_list_for_context.select_row(Some(&row));

            let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

            let save = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Save Note"),
                false,
            );
            let popover_for_save = popover.clone();
            let save_button_for_menu = save_memo_btn_for_context.clone();
            save.connect_clicked(move |_| {
                popover_for_save.popdown();
                save_button_for_menu.emit_clicked();
            });
            menu_box.append(&save);

            let export = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Export as iCalendar"),
                false,
            );
            let popover_for_export = popover.clone();
            let export_button_for_menu = export_memo_btn_for_context.clone();
            export.connect_clicked(move |_| {
                popover_for_export.popdown();
                export_button_for_menu.emit_clicked();
            });
            menu_box.append(&export);

            menu_box.append(&crate::views::create_context_menu_separator());

            let delete = crate::views::create_context_menu_button(
                None,
                &crate::i18n::gettext("Delete Note"),
                true,
            );
            let popover_for_delete = popover.clone();
            let delete_button_for_menu = delete_memo_btn_for_context.clone();
            delete.connect_clicked(move |_| {
                popover_for_delete.popdown();
                delete_button_for_menu.emit_clicked();
            });
            menu_box.append(&delete);

            popover.set_parent(&memo_list_for_context);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        memo_list.add_controller(context_click);

        let memo_key_controller = gtk::EventControllerKey::new();
        let open_for_key = open_memo_btn.clone();
        let copy_for_key = copy_memo_btn.clone();
        let cut_for_key = cut_memo_btn.clone();
        let paste_for_key = paste_memo_btn.clone();
        let delete_for_key = delete_memo_btn.clone();
        let print_for_key = print_memo_btn.clone();
        let search_bar_for_key = memo_search_bar.clone();
        let select_all_for_key = select_all_memo_btn.clone();
        let sidebar_toggle_for_key = sidebar_toggle.clone();
        let preview_toggle_for_key = preview_toggle.clone();
        let save_memo_btn_for_key = save_memo_btn.clone();
        let btn_add_memo_for_key = btn_add_memo.clone();
        memo_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_key.set_active(!sidebar_toggle_for_key.is_active());
                return glib::Propagation::Stop;
            }
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || modifiers.contains(gtk::gdk::ModifierType::META_MASK);
            if control && (key == gtk::gdk::Key::f || key == gtk::gdk::Key::F) {
                let is_active = search_bar_for_key.is_search_mode();
                search_bar_for_key.set_search_mode(!is_active);
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::p || key == gtk::gdk::Key::P) {
                print_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M) {
                preview_toggle_for_key.set_active(!preview_toggle_for_key.is_active());
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::s || key == gtk::gdk::Key::S) {
                save_memo_btn_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::n || key == gtk::gdk::Key::N) {
                btn_add_memo_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::a || key == gtk::gdk::Key::A) {
                select_all_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::c || key == gtk::gdk::Key::C) {
                copy_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::x || key == gtk::gdk::Key::X) {
                cut_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::v || key == gtk::gdk::Key::V) {
                paste_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Delete {
                delete_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Return {
                open_for_key.emit_clicked();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        memo_list.add_controller(memo_key_controller);

        let page_key_controller = gtk::EventControllerKey::new();
        let preview_toggle_for_page = preview_toggle.clone();
        let sidebar_toggle_for_page = sidebar_toggle.clone();
        let save_memo_btn_for_page = save_memo_btn.clone();
        let btn_add_memo_for_page = btn_add_memo.clone();
        let print_for_page = print_memo_btn.clone();
        let search_bar_for_page = memo_search_bar.clone();
        page_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            if key == gtk::gdk::Key::F9 {
                sidebar_toggle_for_page.set_active(!sidebar_toggle_for_page.is_active());
                return glib::Propagation::Stop;
            }
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || modifiers.contains(gtk::gdk::ModifierType::META_MASK);
            if control && (key == gtk::gdk::Key::f || key == gtk::gdk::Key::F) {
                let is_active = search_bar_for_page.is_search_mode();
                search_bar_for_page.set_search_mode(!is_active);
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::p || key == gtk::gdk::Key::P) {
                print_for_page.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::m || key == gtk::gdk::Key::M) {
                preview_toggle_for_page.set_active(!preview_toggle_for_page.is_active());
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::s || key == gtk::gdk::Key::S) {
                save_memo_btn_for_page.emit_clicked();
                return glib::Propagation::Stop;
            }
            if control && (key == gtk::gdk::Key::n || key == gtk::gdk::Key::N) {
                btn_add_memo_for_page.emit_clicked();
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

        let memo_text_key_controller = gtk::EventControllerKey::new();
        let save_memo_btn_for_text = save_memo_btn.clone();
        memo_text_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || modifiers.contains(gtk::gdk::ModifierType::META_MASK);
            if control && (key == gtk::gdk::Key::s || key == gtk::gdk::Key::S) {
                save_memo_btn_for_text.emit_clicked();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        memo_text.add_controller(memo_text_key_controller);

        // Header Draw Func
        {
            let page_weak = self.downgrade();
            memo_table_header.set_draw_func(move |_, cr, width, height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let hovered = page.imp().memo_table_hovered_header.get();
                let sort = page.imp().memo_sort.get();
                let descending = page.imp().memo_sort_descending.get();
                let is_dark = adw::StyleManager::default().is_dark();
                if let Some(layout) = layout_ref.as_ref() {
                    draw_memo_table_header(cr, layout, width as f64, height as f64, sort, descending, hovered, is_dark);
                }
            });
        }

        // Header Click Gesture
        {
            let header_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let header_clone = memo_table_header.clone();
            let memo_list_for_header = memo_list.clone();
            let memo_records_for_header = Rc::clone(&memo_records);
            let empty_state_for_header = empty_state.clone();
            header_click.connect_pressed(move |gesture, _n_press, x, _y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                if let Some(col) = hit_test_memo_table_header(layout, x) {
                    let current_sort = page.imp().memo_sort.get();
                    let current_descending = page.imp().memo_sort_descending.get();
                    let new_sort = match col {
                        MemoTableColumn::Title => MemoSort::Title,
                        MemoTableColumn::Source => MemoSort::Source,
                        MemoTableColumn::Updated => MemoSort::Updated,
                        _ => current_sort,
                    };
                    let new_descending = if current_sort == new_sort {
                        !current_descending
                    } else {
                        false
                    };
                    page.imp().memo_sort.set(new_sort);
                    page.imp().memo_sort_descending.set(new_descending);
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_memo_sort_field(new_sort.setting_value());
                        settings.set_memo_sort_descending(new_descending);
                    }
                    header_clone.queue_draw();
                    refresh_memo_filter_view(&page, &memo_list_for_header, &memo_records_for_header, &empty_state_for_header);
                }
            });
            memo_table_header.add_controller(header_click);
        }

        // Header Motion Controller
        {
            let header_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let header_clone = memo_table_header.clone();
            header_motion.connect_motion(move |_, x, _y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let col = layout_ref.as_ref().and_then(|l| hit_test_memo_table_header(l, x));
                if page.imp().memo_table_hovered_header.get() != col {
                    page.imp().memo_table_hovered_header.set(col);
                    header_clone.queue_draw();
                }
            });
            let page_weak = self.downgrade();
            let header_clone = memo_table_header.clone();
            header_motion.connect_leave(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                if page.imp().memo_table_hovered_header.get().is_some() {
                    page.imp().memo_table_hovered_header.set(None);
                    header_clone.queue_draw();
                }
            });
            memo_table_header.add_controller(header_motion);
        }

        // Canvas Draw Func
        {
            let page_weak = self.downgrade();
            memo_table_canvas.set_draw_func(move |_, cr, width, height| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let selection = page.imp().memo_selection.borrow();
                let hovered = page.imp().memo_table_hovered_row.get();
                let is_dark = adw::StyleManager::default().is_dark();
                if let Some(layout) = layout_ref.as_ref() {
                    draw_memo_table_canvas(cr, layout, &selection, width as f64, height as f64, hovered, is_dark);
                }
            });
            let canvas_for_theme = memo_table_canvas.clone();
            let header_for_theme = memo_table_header.clone();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                canvas_for_theme.queue_draw();
                header_for_theme.queue_draw();
            });
        }

        // Canvas Primary Click Gesture
        {
            let canvas_click = gtk::GestureClick::new();
            let page_weak = self.downgrade();
            let canvas_clone = memo_table_canvas.clone();
            let memo_list_for_click = memo_list.clone();
            let memo_records_for_click = Rc::clone(&memo_records);
            let selected_memo_for_click = Rc::clone(&selected_memo);
            let memo_mutation_generation_for_click = Rc::clone(&memo_mutation_generation);
            let memo_alarm_editor_for_click = memo_alarm_editor.clone();
            let memo_detail_stack_for_click = memo_detail_stack.clone();
            let memo_title_for_click = memo_title.clone();
            let memo_categories_for_click = memo_categories.clone();
            let memo_meta_for_click = memo_meta.clone();
            let memo_text_for_click = memo_text.clone();
            let open_memo_btn_for_click = open_memo_btn.clone();

            canvas_click.connect_pressed(move |gesture, n_press, _x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let Some(row_idx) = hit_test_memo_table(layout, y) else { return; };

                canvas_clone.grab_focus();

                if n_press == 2 {
                    open_memo_btn_for_click.emit_clicked();
                } else if n_press == 1 {
                    let current_event = gesture.current_event();
                    let state = current_event.as_ref().map(|e| e.modifier_state()).unwrap_or(gdk::ModifierType::empty());
                    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                    let ctrl_or_cmd = state.contains(gdk::ModifierType::CONTROL_MASK) || state.contains(gdk::ModifierType::META_MASK);

                    let clicked_id = layout.rows[row_idx].identity.clone();
                    let mut current_sel = page.imp().memo_selection.borrow().clone();

                    if shift {
                        let anchor = page.imp().memo_table_anchor_row.get().unwrap_or(row_idx);
                        let start = anchor.min(row_idx);
                        let end = anchor.max(row_idx);
                        let mut range_sel = Vec::new();
                        for i in start..=end {
                            if let Some(r) = layout.rows.get(i) {
                                range_sel.push(r.identity.clone());
                            }
                        }
                        current_sel.set_selected(range_sel);
                        current_sel.set_focus(Some(clicked_id.clone()));
                        page.imp().memo_selection.replace(current_sel.clone());
                    } else if ctrl_or_cmd {
                        let mut selected = current_sel.selected().to_vec();
                        if let Some(pos) = selected.iter().position(|id| id == &clicked_id) {
                            selected.remove(pos);
                        } else {
                            selected.push(clicked_id.clone());
                        }
                        current_sel.set_selected(selected);
                        current_sel.set_focus(Some(clicked_id.clone()));
                        page.imp().memo_table_anchor_row.set(Some(row_idx));
                        page.imp().memo_selection.replace(current_sel.clone());
                    } else {
                        page.imp().memo_table_anchor_row.set(Some(row_idx));
                        current_sel.set_selected(vec![clicked_id.clone()]);
                        current_sel.set_focus(Some(clicked_id.clone()));
                        page.imp().memo_selection.replace(current_sel.clone());
                    }

                    unsafe {
                        memo_list_for_click.set_data("canvas-selection", current_sel.selected().to_vec());
                        if let Some(focus) = current_sel.focus() {
                            memo_list_for_click.set_data("canvas-focus", focus.clone());
                        }
                    }

                    memo_mutation_generation_for_click.set(memo_mutation_generation_for_click.get().wrapping_add(1));
                    if current_sel.len() == 1 {
                        if let Some(memo) = memo_records_for_click.borrow().iter().find(|m| m.source_uid == clicked_id.source_uid && m.uid == clicked_id.uid).cloned() {
                            *selected_memo_for_click.borrow_mut() = Some(memo.clone());
                            memo_title_for_click.set_text(&memo.summary);
                            memo_categories_for_click.set_text(&component_categories(&memo.component));
                            memo_meta_for_click.set_label(&format!("VJOURNAL · {}", memo.updated));
                            memo_text_for_click.buffer().set_text(&memo.description);
                            memo_alarm_editor_for_click.set_component(Some(&memo.component), &memo.summary);
                            memo_detail_stack_for_click.set_visible_child_name("content");
                        }
                    } else if current_sel.is_empty() {
                        *selected_memo_for_click.borrow_mut() = None;
                        memo_alarm_editor_for_click.set_component(None, "");
                        memo_detail_stack_for_click.set_visible_child_name("empty");
                    }

                    sync_memo_surface_selection(&page, &memo_list_for_click);
                    update_memo_surface_widgets_from_page(&page, &memo_list_for_click, &memo_records_for_click);
                    refresh_memo_preview(&page, &memo_list_for_click, &memo_records_for_click);
                    canvas_clone.queue_draw();
                }
            });
            memo_table_canvas.add_controller(canvas_click);
        }

        // Canvas Secondary Click Gesture (Context Menu)
        {
            let canvas_secondary_click = gtk::GestureClick::new();
            canvas_secondary_click.set_button(gdk::BUTTON_SECONDARY);
            let page_weak = self.downgrade();
            let canvas_clone = memo_table_canvas.clone();
            let memo_list_for_ctx = memo_list.clone();
            let memo_records_for_ctx = Rc::clone(&memo_records);
            let selected_memo_for_ctx = Rc::clone(&selected_memo);
            let memo_mutation_generation_for_ctx = Rc::clone(&memo_mutation_generation);
            let memo_alarm_editor_for_ctx = memo_alarm_editor.clone();
            let memo_detail_stack_for_ctx = memo_detail_stack.clone();
            let memo_title_for_ctx = memo_title.clone();
            let memo_categories_for_ctx = memo_categories.clone();
            let memo_meta_for_ctx = memo_meta.clone();
            let memo_text_for_ctx = memo_text.clone();
            let save_btn_for_ctx = save_memo_btn.clone();
            let export_btn_for_ctx = export_memo_btn.clone();
            let delete_btn_for_ctx = delete_memo_btn.clone();

            canvas_secondary_click.connect_pressed(move |gesture, _n_press, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return; };
                let Some(row_idx) = hit_test_memo_table(layout, y) else { return; };

                let clicked_id = layout.rows[row_idx].identity.clone();
                let mut current_sel = page.imp().memo_selection.borrow().clone();
                if !current_sel.contains(&clicked_id) {
                    current_sel.set_selected(vec![clicked_id.clone()]);
                    current_sel.set_focus(Some(clicked_id.clone()));
                    page.imp().memo_table_anchor_row.set(Some(row_idx));
                    page.imp().memo_selection.replace(current_sel.clone());
                    unsafe {
                        memo_list_for_ctx.set_data("canvas-selection", current_sel.selected().to_vec());
                        memo_list_for_ctx.set_data("canvas-focus", clicked_id.clone());
                    }
                    memo_mutation_generation_for_ctx.set(memo_mutation_generation_for_ctx.get().wrapping_add(1));
                    if let Some(memo) = memo_records_for_ctx.borrow().iter().find(|m| m.source_uid == clicked_id.source_uid && m.uid == clicked_id.uid).cloned() {
                        *selected_memo_for_ctx.borrow_mut() = Some(memo.clone());
                        memo_title_for_ctx.set_text(&memo.summary);
                        memo_categories_for_ctx.set_text(&component_categories(&memo.component));
                        memo_meta_for_ctx.set_label(&format!("VJOURNAL · {}", memo.updated));
                        memo_text_for_ctx.buffer().set_text(&memo.description);
                        memo_alarm_editor_for_ctx.set_component(Some(&memo.component), &memo.summary);
                        memo_detail_stack_for_ctx.set_visible_child_name("content");
                    }
                    sync_memo_surface_selection(&page, &memo_list_for_ctx);
                    update_memo_surface_widgets_from_page(&page, &memo_list_for_ctx, &memo_records_for_ctx);
                    refresh_memo_preview(&page, &memo_list_for_ctx, &memo_records_for_ctx);
                    canvas_clone.queue_draw();
                }

                let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
                let (popover, menu_box) = crate::views::create_context_popover(Some(&rect));

                let save = crate::views::create_context_menu_button(
                    None,
                    &crate::i18n::gettext("Save Note"),
                    false,
                );
                let pop_save = popover.clone();
                let save_btn = save_btn_for_ctx.clone();
                save.connect_clicked(move |_| {
                    pop_save.popdown();
                    save_btn.emit_clicked();
                });
                menu_box.append(&save);

                let export = crate::views::create_context_menu_button(
                    None,
                    &crate::i18n::gettext("Export as iCalendar"),
                    false,
                );
                let pop_exp = popover.clone();
                let exp_btn = export_btn_for_ctx.clone();
                export.connect_clicked(move |_| {
                    pop_exp.popdown();
                    exp_btn.emit_clicked();
                });
                menu_box.append(&export);

                menu_box.append(&crate::views::create_context_menu_separator());

                let delete = crate::views::create_context_menu_button(
                    None,
                    &crate::i18n::gettext("Delete Note"),
                    true,
                );
                let pop_del = popover.clone();
                let del_btn = delete_btn_for_ctx.clone();
                delete.connect_clicked(move |_| {
                    pop_del.popdown();
                    del_btn.emit_clicked();
                });
                menu_box.append(&delete);

                popover.set_parent(&canvas_clone);
                let pop_weak = popover.downgrade();
                popover.connect_closed(move |_| {
                    if let Some(p) = pop_weak.upgrade() {
                        p.unparent();
                    }
                });
                popover.popup();
            });
            memo_table_canvas.add_controller(canvas_secondary_click);
        }

        // Canvas Motion Controller
        {
            let canvas_motion = gtk::EventControllerMotion::new();
            let page_weak = self.downgrade();
            let canvas_clone = memo_table_canvas.clone();
            canvas_motion.connect_motion(move |_, _x, y| {
                let Some(page) = page_weak.upgrade() else { return; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let hovered_idx = layout_ref.as_ref().and_then(|l| hit_test_memo_table(l, y));
                if page.imp().memo_table_hovered_row.get() != hovered_idx {
                    page.imp().memo_table_hovered_row.set(hovered_idx);
                    canvas_clone.queue_draw();
                }
            });
            let page_weak = self.downgrade();
            let canvas_clone = memo_table_canvas.clone();
            canvas_motion.connect_leave(move |_| {
                let Some(page) = page_weak.upgrade() else { return; };
                if page.imp().memo_table_hovered_row.get().is_some() {
                    page.imp().memo_table_hovered_row.set(None);
                    canvas_clone.queue_draw();
                }
            });
            memo_table_canvas.add_controller(canvas_motion);
        }

        // Canvas Key Controller
        {
            let key_controller = gtk::EventControllerKey::new();
            let page_weak = self.downgrade();
            let canvas_clone = memo_table_canvas.clone();
            let memo_list_for_key = memo_list.clone();
            let memo_records_for_key = Rc::clone(&memo_records);
            let selected_memo_for_key = Rc::clone(&selected_memo);
            let memo_mutation_gen_for_key = Rc::clone(&memo_mutation_generation);
            let memo_alarm_editor_for_key = memo_alarm_editor.clone();
            let memo_detail_stack_for_key = memo_detail_stack.clone();
            let memo_title_for_key = memo_title.clone();
            let memo_categories_for_key = memo_categories.clone();
            let memo_meta_for_key = memo_meta.clone();
            let memo_text_for_key = memo_text.clone();
            let open_btn_for_key = open_memo_btn.clone();
            let delete_btn_for_key = delete_memo_btn.clone();
            let select_all_btn_for_key = select_all_memo_btn.clone();
            let copy_btn_for_key = copy_memo_btn.clone();
            let cut_btn_for_key = cut_memo_btn.clone();
            let paste_btn_for_key = paste_memo_btn.clone();
            let save_btn_for_key = save_memo_btn.clone();
            let btn_add_memo_for_key = btn_add_memo.clone();
            let sidebar_toggle_for_key = sidebar_toggle.clone();
            let preview_toggle_for_key = preview_toggle.clone();

            key_controller.connect_key_pressed(move |_, keyval, _, state| {
                if keyval == gdk::Key::F9 {
                    sidebar_toggle_for_key.set_active(!sidebar_toggle_for_key.is_active());
                    return glib::Propagation::Stop;
                }
                let control = state.contains(gdk::ModifierType::CONTROL_MASK)
                    || state.contains(gdk::ModifierType::META_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);

                if control && (keyval == gdk::Key::m || keyval == gdk::Key::M) {
                    preview_toggle_for_key.set_active(!preview_toggle_for_key.is_active());
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::s || keyval == gdk::Key::S) {
                    save_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::n || keyval == gdk::Key::N) {
                    btn_add_memo_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::a || keyval == gdk::Key::A) {
                    select_all_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::c || keyval == gdk::Key::C) {
                    copy_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::x || keyval == gdk::Key::X) {
                    cut_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if control && (keyval == gdk::Key::v || keyval == gdk::Key::V) {
                    paste_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if keyval == gdk::Key::Delete {
                    delete_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }
                if keyval == gdk::Key::Return {
                    open_btn_for_key.emit_clicked();
                    return glib::Propagation::Stop;
                }

                let Some(page) = page_weak.upgrade() else { return glib::Propagation::Proceed; };
                let layout_ref = page.imp().memo_table_layout.borrow();
                let Some(layout) = layout_ref.as_ref() else { return glib::Propagation::Proceed; };
                if layout.rows.is_empty() { return glib::Propagation::Proceed; }

                let current_sel = page.imp().memo_selection.borrow().clone();
                let current_idx = current_sel.focus().and_then(|f| layout.rows.iter().position(|r| r.identity == *f)).unwrap_or(0);
                let anchor = page.imp().memo_table_anchor_row.get().unwrap_or(current_idx);

                let next_idx = match keyval {
                    gdk::Key::Up => current_idx.saturating_sub(1),
                    gdk::Key::Down => (current_idx + 1).min(layout.rows.len() - 1),
                    gdk::Key::Home => 0,
                    gdk::Key::End => layout.rows.len() - 1,
                    gdk::Key::Page_Up => current_idx.saturating_sub(10),
                    gdk::Key::Page_Down => (current_idx + 10).min(layout.rows.len() - 1),
                    _ => return glib::Propagation::Proceed,
                };

                let mut new_sel = MemoSelection::default();
                let target_id = layout.rows[next_idx].identity.clone();
                if shift {
                    let start = anchor.min(next_idx);
                    let end = anchor.max(next_idx);
                    let range: Vec<MemoIdentity> = layout.rows[start..=end].iter().map(|r| r.identity.clone()).collect();
                    new_sel.set_selected(range);
                    new_sel.set_focus(Some(target_id.clone()));
                } else {
                    page.imp().memo_table_anchor_row.set(Some(next_idx));
                    new_sel.set_selected(vec![target_id.clone()]);
                    new_sel.set_focus(Some(target_id.clone()));
                }
                page.imp().memo_selection.replace(new_sel.clone());
                unsafe {
                    memo_list_for_key.set_data("canvas-selection", new_sel.selected().to_vec());
                    if let Some(focus) = new_sel.focus() {
                        memo_list_for_key.set_data("canvas-focus", focus.clone());
                    }
                }
                if new_sel.len() == 1 {
                    memo_mutation_gen_for_key.set(memo_mutation_gen_for_key.get().wrapping_add(1));
                    if let Some(memo) = memo_records_for_key.borrow().iter().find(|m| m.source_uid == target_id.source_uid && m.uid == target_id.uid).cloned() {
                        *selected_memo_for_key.borrow_mut() = Some(memo.clone());
                        memo_title_for_key.set_text(&memo.summary);
                        memo_categories_for_key.set_text(&component_categories(&memo.component));
                        memo_meta_for_key.set_label(&format!("VJOURNAL · {}", memo.updated));
                        memo_text_for_key.buffer().set_text(&memo.description);
                        memo_alarm_editor_for_key.set_component(Some(&memo.component), &memo.summary);
                        memo_detail_stack_for_key.set_visible_child_name("content");
                        let speech = format!("{}, {}", memo.summary, memo.updated);
                        set_accessible_description(&canvas_clone, &speech);
                        announce_to_screen_reader(&canvas_clone, &speech, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                }
                sync_memo_surface_selection(&page, &memo_list_for_key);
                update_memo_surface_widgets_from_page(&page, &memo_list_for_key, &memo_records_for_key);
                refresh_memo_preview(&page, &memo_list_for_key, &memo_records_for_key);
                canvas_clone.queue_draw();

                glib::Propagation::Stop
            });
            memo_table_canvas.add_controller(key_controller);
        }

        let memo_records_for_selection = Rc::clone(&memo_records);
        let selected_memo_for_selection = Rc::clone(&selected_memo);
        let memo_mutation_generation_for_selection = Rc::clone(&memo_mutation_generation);
        let memo_alarm_editor_for_selection = memo_alarm_editor.clone();
        let memo_detail_stack_for_selection = memo_detail_stack.clone();
        memo_list.connect_row_selected(glib::clone!(
            #[weak]
            memo_title,
            #[weak]
            memo_categories,
            #[weak]
            memo_meta,
            #[weak]
            memo_text,
            #[strong]
            selected_memo_for_selection,
            #[strong]
            memo_mutation_generation_for_selection,
            #[strong]
            memo_alarm_editor_for_selection,
            #[strong]
            memo_detail_stack_for_selection,
            move |_, row| {
                memo_mutation_generation_for_selection
                    .set(memo_mutation_generation_for_selection.get().wrapping_add(1));
                let Some(row) = row else {
                    *selected_memo_for_selection.borrow_mut() = None;
                    memo_alarm_editor_for_selection.set_component(None, "");
                    memo_detail_stack_for_selection.set_visible_child_name("empty");
                    return;
                };
                let (source_uid, uid) = unsafe {
                    let source_uid = row
                        .data::<String>("memo-source-uid")
                        .map(|value| value.as_ref().clone())
                        .unwrap_or_default();
                    let uid = row
                        .data::<String>("memo-id")
                        .map(|value| value.as_ref().clone())
                        .unwrap_or_default();
                    (source_uid, uid)
                };
                let Some(memo) = memo_records_for_selection
                    .borrow()
                    .iter()
                    .find(|memo| memo.source_uid == source_uid && memo.uid == uid)
                    .cloned()
                else {
                    memo_detail_stack_for_selection.set_visible_child_name("empty");
                    return;
                };
                *selected_memo_for_selection.borrow_mut() = Some(memo.clone());
                memo_title.set_text(&memo.summary);
                memo_categories.set_text(&component_categories(&memo.component));
                memo_meta.set_label(&format!("VJOURNAL · {}", memo.updated));
                memo_text.buffer().set_text(&memo.description);
                memo_alarm_editor_for_selection.set_component(Some(&memo.component), &memo.summary);
                memo_detail_stack_for_selection.set_visible_child_name("content");
            }
        ));

        let page_weak_for_surface_selection = self.downgrade();
        let memo_list_for_surface_selection = memo_list.clone();
        let memo_records_for_surface_selection = Rc::clone(&memo_records);
        let save_memo_btn_for_surface_selection = save_memo_btn.clone();
        let export_memo_btn_for_surface_selection = export_memo_btn.clone();
        let delete_memo_btn_for_surface_selection = delete_memo_btn.clone();
        let open_memo_btn_for_surface_selection = open_memo_btn.clone();
        let forward_memo_btn_for_surface_selection = forward_memo_btn.clone();
        let open_url_memo_btn_for_surface_selection = open_url_memo_btn.clone();
        let print_memo_btn_for_surface_selection = print_memo_btn.clone();
        let save_as_memo_btn_for_surface_selection = save_as_memo_btn.clone();
        let copy_memo_btn_for_surface_selection = copy_memo_btn.clone();
        let cut_memo_btn_for_surface_selection = cut_memo_btn.clone();
        let paste_memo_btn_for_surface_selection = paste_memo_btn.clone();
        let select_all_memo_btn_for_surface_selection = select_all_memo_btn.clone();
        memo_list.connect_selected_rows_changed(move |_| {
            let Some(page) = page_weak_for_surface_selection.upgrade() else {
                return;
            };
            sync_memo_surface_selection(&page, &memo_list_for_surface_selection);
            update_memo_surface_widgets(
                &page,
                &memo_list_for_surface_selection,
                &memo_records_for_surface_selection,
                &save_memo_btn_for_surface_selection,
                &export_memo_btn_for_surface_selection,
                &delete_memo_btn_for_surface_selection,
                &open_memo_btn_for_surface_selection,
                &forward_memo_btn_for_surface_selection,
                &open_url_memo_btn_for_surface_selection,
                &print_memo_btn_for_surface_selection,
                &save_as_memo_btn_for_surface_selection,
                &copy_memo_btn_for_surface_selection,
                &cut_memo_btn_for_surface_selection,
                &paste_memo_btn_for_surface_selection,
                &select_all_memo_btn_for_surface_selection,
            );
        });

        let page_weak_for_preview_toggle = self.downgrade();
        let memo_list_for_preview_toggle = memo_list.clone();
        let memo_records_for_preview_toggle = Rc::clone(&memo_records);
        let memo_preview_box_for_toggle = memo_preview_box.clone();
        preview_toggle.connect_toggled(move |toggle| {
            let Some(page) = page_weak_for_preview_toggle.upgrade() else {
                return;
            };
            page.imp()
                .memo_preview_state
                .borrow_mut()
                .set_visible(toggle.is_active());
            memo_preview_box_for_toggle.set_visible(toggle.is_active());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_preview_visible(toggle.is_active());
            }
            refresh_memo_preview(
                &page,
                &memo_list_for_preview_toggle,
                &memo_records_for_preview_toggle,
            );
            if let Some(win) = crate::window::find_meridian_window(&page) {
                win.sync_right_sidebar_toggle();
            }
        });

        let page_weak_for_preview_orientation = self.downgrade();
        let memo_list_for_preview_orientation = memo_list.clone();
        let memo_records_for_preview_orientation = Rc::clone(&memo_records);
        preview_orientation_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_preview_orientation.upgrade() else {
                return;
            };
            let orientation = if selector.selected() == 1 {
                MemoPreviewOrientation::Vertical
            } else {
                MemoPreviewOrientation::Classic
            };
            page.imp()
                .memo_preview_state
                .borrow_mut()
                .set_orientation(orientation);
            apply_memo_preview_paned_layout(&page, orientation);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_preview_orientation(orientation.setting_value());
            }
            refresh_memo_preview(
                &page,
                &memo_list_for_preview_orientation,
                &memo_records_for_preview_orientation,
            );
        });

        let memo_text_for_activate = memo_text.clone();
        let page_for_activate = self.downgrade();
        memo_list.connect_row_activated(move |_, _| {
            memo_text_for_activate.grab_focus();
            if let Some(page) = page_for_activate.upgrade() {
                if page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_memos_layout_visibility();
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_back_button();
                    }
                }
            }
        });

        let click_for_mobile_memo = gtk::GestureClick::new();
        let page_for_memo_click = self.downgrade();
        click_for_mobile_memo.connect_released(move |_, _, _, _| {
            if let Some(page) = page_for_memo_click.upgrade() {
                if page.imp().is_mobile.get() {
                    page.imp().is_detail_shown.set(true);
                    page.sync_memos_layout_visibility();
                    if let Some(win) = crate::window::find_meridian_window(&page) {
                        win.sync_back_button();
                    }
                }
            }
        });
        memo_list.add_controller(click_for_mobile_memo);

        let memo_list_for_load = memo_list.clone();
        let empty_state_for_load = empty_state.clone();
        let memo_meta_for_load = memo_meta.clone();
        let memo_records_for_load = Rc::clone(&memo_records);
        let page_weak_for_load = self.downgrade();
        let load_generation = self.imp().memo_refresh_generation.get().wrapping_add(1);
        self.imp().memo_refresh_generation.set(load_generation);
        let (memo_sender, memo_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = load_snapshot_prefer_eds();
            if let Err(error) = memo_sender.send_blocking(result) {
                tracing::debug!(%error, "Memo snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match memo_receiver.recv().await {
                Ok(result) => {
                    let Some(page) = page_weak_for_load.upgrade() else {
                        return;
                    };
                    if page.imp().memo_refresh_generation.get() != load_generation {
                        tracing::debug!(
                            load_generation,
                            current_generation = page.imp().memo_refresh_generation.get(),
                            "Discarding stale memo snapshot"
                        );
                        return;
                    }
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Memo data loaded with a backend warning");
                    }
                    let memos = result.snapshot.memos;
                    let memo_count = memos.len();
                    replace_memo_snapshot(
                        &memo_list_for_load,
                        Rc::clone(&memo_records_for_load),
                        memos,
                    );
                    if memo_count == 0 {
                        let empty_text = match result.origin {
                            CalendarDataOrigin::Eds => crate::i18n::gettext("No synchronized memos found"),
                            CalendarDataOrigin::Graph => {
                                crate::i18n::gettext("No VJOURNAL data found from Microsoft Graph")
                            }
                            CalendarDataOrigin::Local => crate::i18n::gettext("No local VJOURNAL data found"),
                        };
                        empty_state_for_load.set_label(&empty_text);
                    } else {
                        empty_state_for_load.set_visible(false);
                        let origin = match result.origin {
                            CalendarDataOrigin::Eds => crate::i18n::gettext("synchronized memo store"),
                            CalendarDataOrigin::Graph => "Microsoft Graph".to_string(),
                            CalendarDataOrigin::Local => crate::i18n::gettext("local iCalendar"),
                        };
                        memo_meta_for_load
                            .set_label(&crate::i18n::ngettext(
                                "{} memo loaded from {origin}",
                                "{} memos loaded from {origin}",
                                memo_count as u32,
                            ).replace("{}", &memo_count.to_string()).replace("{origin}", &origin));
                    }
                    refresh_memo_category_model(&page, &memo_list_for_load);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_load,
                        &memo_records_for_load,
                        &empty_state_for_load,
                    );
                }
                Err(error) => tracing::debug!(%error, "Memo snapshot unavailable"),
            }
        });

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let memo_list_for_monitor = memo_list.clone();
            let memo_records_for_monitor = Rc::clone(&memo_records);
            let empty_state_for_monitor = empty_state.clone();
            let selected_memo_for_monitor = Rc::clone(&selected_memo);
            let memo_title_for_monitor = memo_title.clone();
            let memo_categories_for_monitor = memo_categories.clone();
            let memo_meta_for_monitor = memo_meta.clone();
            let memo_text_for_monitor = memo_text.clone();
            let memo_alarm_editor_for_monitor = memo_alarm_editor.clone();
            let page_weak_for_monitor = self.downgrade();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    if matches!(&event, ECalSyncEvent::Change(_)) {
                        if let Some(page) = page_weak_for_monitor.upgrade() {
                            let generation =
                                page.imp().memo_refresh_generation.get().wrapping_add(1);
                            page.imp().memo_refresh_generation.set(generation);
                        }
                    }
                    match event {
                        ECalSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "ECal memo signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => {
                                let removed = remove_memo_row(
                                    &memo_list_for_monitor,
                                    Rc::clone(&memo_records_for_monitor),
                                    &change.source_uid,
                                    &change.uid,
                                );
                                if removed
                                    && selected_memo_for_monitor.borrow().as_ref().is_some_and(
                                        |memo| {
                                            memo.source_uid == change.source_uid
                                                && memo.uid == change.uid
                                        },
                                    )
                                {
                                    *selected_memo_for_monitor.borrow_mut() = None;
                                    memo_title_for_monitor.set_text("");
                                    memo_categories_for_monitor.set_text("");
                                    memo_meta_for_monitor.set_label(&crate::i18n::gettext("No memo selected"));
                                    memo_text_for_monitor
                                        .buffer()
                                        .set_text(&crate::i18n::gettext("No memo data available yet."));
                                    memo_alarm_editor_for_monitor.set_component(None, "");
                                    if let Some(page) = page_weak_for_monitor.upgrade() {
                                        if let Some(stack) = page.imp().memo_detail_stack.borrow().as_ref() {
                                            stack.set_visible_child_name("empty");
                                        }
                                    }
                                }
                            }
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                if let Some(ECalRecord::Memo(memo)) = parse_change_record(&change) {
                                    upsert_memo_row(
                                        &memo_list_for_monitor,
                                        Rc::clone(&memo_records_for_monitor),
                                        memo.clone(),
                                    );
                                    if selected_memo_for_monitor.borrow().as_ref().is_some_and(
                                        |selected| {
                                            selected.source_uid == memo.source_uid
                                                && selected.uid == memo.uid
                                        },
                                    ) {
                                        *selected_memo_for_monitor.borrow_mut() =
                                            Some(memo.clone());
                                        memo_title_for_monitor.set_text(&memo.summary);
                                        memo_categories_for_monitor
                                            .set_text(&component_categories(&memo.component));
                                        memo_meta_for_monitor
                                            .set_label(&format!("VJOURNAL · {}", memo.updated));
                                        memo_text_for_monitor.buffer().set_text(&memo.description);
                                        memo_alarm_editor_for_monitor
                                            .set_component(Some(&memo.component), &memo.summary);
                                    }
                                }
                            }
                        },
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal memo signal monitor stopped");
                            break;
                        }
                    }
                    empty_state_for_monitor
                        .set_visible(memo_records_for_monitor.borrow().is_empty());
                    if let Some(page) = page_weak_for_monitor.upgrade() {
                        refresh_memo_category_model(&page, &memo_list_for_monitor);
                        refresh_memo_filter_view(
                            &page,
                            &memo_list_for_monitor,
                            &memo_records_for_monitor,
                            &empty_state_for_monitor,
                        );
                    }
                }
            });
        }

        update_memo_surface_widgets(
            self,
            &memo_list,
            &memo_records,
            &save_memo_btn,
            &export_memo_btn,
            &delete_memo_btn,
            &open_memo_btn,
            &forward_memo_btn,
            &open_url_memo_btn,
            &print_memo_btn,
            &save_as_memo_btn,
            &copy_memo_btn,
            &cut_memo_btn,
            &paste_memo_btn,
            &select_all_memo_btn,
        );

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        content_box.append(&memos_toolbar_scroll);
        content_box.append(&memo_search_bar);
        content_box.append(&memo_preview_paned);

        paned.set_end_child(Some(&content_box));
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(true);

        let sidebar_position = AppSettings::try_new()
            .map(|settings| settings.mail_sidebar_position())
            .unwrap_or(260)
            .clamp(crate::window::MIN_SIDEBAR_WIDTH, crate::window::MAX_SIDEBAR_WIDTH);
        paned.set_position(sidebar_position);
        if let Some(settings) = AppSettings::try_new() {
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
                if settings.mail_sidebar_position() != clamped {
                    settings.set_mail_sidebar_position(clamped);
                }
            });
        }
        let root_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .hexpand(true)
            .build();
        root_box.append(&paned);
        root_box.set_parent(self);
    }

    fn open_memo_dialog(
        &self,
        memo_list: gtk::ListBox,
        memo_records: Rc<RefCell<Vec<MemoRecord>>>,
        empty_state: gtk::Label,
        memo_to_edit: Option<MemoRecord>,
    ) {
        let editing = memo_to_edit.is_some();
        let initial_component = memo_to_edit
            .as_ref()
            .map(|memo| memo.component.clone())
            .unwrap_or_else(|| {
                build_memo_component(&crate::i18n::gettext("New Note"), "", "", &[])
                    .expect("the default memo component is valid")
            });
        let initial_draft = match MemoEditorDraft::parse(&initial_component) {
            Ok(draft) => draft,
            Err(error) => {
                tracing::warn!(%error, "Could not create memo editor draft");
                return;
            }
        };
        let draft_for_save = Rc::new(RefCell::new(initial_draft.clone()));
        let dirty = Rc::new(Cell::new(false));
        let operation_in_progress = Rc::new(Cell::new(false));
        let request_generation = Rc::new(Cell::new(0u64));
        let close_confirmed = Rc::new(Cell::new(false));
        let close_prompt_open = Rc::new(Cell::new(false));
        let attachment_loading = Rc::new(Cell::new(false));

        let title_edit = crate::i18n::gettext("Edit Note");
        let title_new = crate::i18n::gettext("New Note");
        let dialog = adw::Window::builder()
            .title(if editing {
                &title_edit
            } else {
                &title_new
            })
            .modal(true)
            .default_width(680)
            .default_height(760)
            .build();
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
        }

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_start_title_buttons(false);
        header_bar.set_show_end_title_buttons(false);
        let cancel_button = gtk::Button::with_label(&crate::i18n::gettext("Cancel"));
        header_bar.pack_start(&cancel_button);
        let save_button = gtk::Button::builder()
            .label(&crate::i18n::gettext("Save"))
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&save_button);
        toolbar_view.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let loading_memo_lists = crate::i18n::gettext("Loading memo lists…");
        let source_model = gtk::StringList::new(&[&loading_memo_lists]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Memo List"))
            .model(&source_model)
            .build();
        let settings_for_default_memo = AppSettings::try_new();
        let source_uids_for_selection = Rc::clone(&source_uids);
        source_row.connect_selected_notify(move |row| {
            let Some(settings) = settings_for_default_memo.as_ref() else {
                return;
            };
            let Some(source_uid) = source_uids_for_selection
                .borrow()
                .get(row.selected() as usize)
                .cloned()
            else {
                return;
            };
            settings.set_default_memo_source_uid(&source_uid);
        });

        let details = adw::PreferencesGroup::builder().title(&crate::i18n::gettext("Details")).build();
        details.add(&source_row);
        let title_entry = adw::EntryRow::builder().title(&crate::i18n::gettext("Summary")).build();
        details.add(&title_entry);
        let dtstart_entry = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("DTSTART (iCalendar value)"))
            .build();
        dtstart_entry.set_tooltip_text(Some(&crate::i18n::gettext("e.g. 20260822 or 20260822T120000Z")));
        details.add(&dtstart_entry);
        let class_entry = adw::EntryRow::builder().title(&crate::i18n::gettext("Class")).build();
        class_entry.set_tooltip_text(Some(&crate::i18n::gettext("e.g. PUBLIC, PRIVATE, or CONFIDENTIAL")));
        details.add(&class_entry);
        let status_entry = adw::EntryRow::builder().title(&crate::i18n::gettext("Status")).build();
        status_entry.set_tooltip_text(Some(&crate::i18n::gettext("RFC5545 STATUS, e.g. DRAFT or FINAL")));
        details.add(&status_entry);
        let url_entry = adw::EntryRow::builder().title(&crate::i18n::gettext("URL")).build();
        details.add(&url_entry);
        let categories_entry = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Categories (comma-separated)"))
            .build();
        categories_entry.set_tooltip_text(Some(&crate::i18n::gettext("e.g. Work, Personal")));
        details.add(&categories_entry);
        let alarm_editor = AlarmEditor::new(Some(&initial_component), &initial_draft.summary);
        let alarm_dirty_gesture = gtk::GestureClick::new();
        let dirty_for_alarm = Rc::clone(&dirty);
        alarm_dirty_gesture.connect_pressed(move |_, _, _, _| dirty_for_alarm.set(true));
        alarm_editor.widget().add_controller(alarm_dirty_gesture);
        details.add(alarm_editor.widget());
        content_box.append(&details);

        title_entry.set_text(&initial_draft.summary);
        dtstart_entry.set_text(initial_draft.dtstart.as_deref().unwrap_or_default());
        class_entry.set_text(initial_draft.class.as_deref().unwrap_or_default());
        status_entry.set_text(initial_draft.status.as_deref().unwrap_or_default());
        url_entry.set_text(initial_draft.url.as_deref().unwrap_or_default());
        categories_entry.set_text(&serialize_categories_vec(&initial_draft.categories));

        let description_group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Description"))
            .build();
        let description_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(180)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        description_view
            .buffer()
            .set_text(&initial_draft.description);
        description_group.add(&description_view);
        content_box.append(&description_group);

        let attachment_group = adw::PreferencesGroup::builder().title(&crate::i18n::gettext("Attachments")).build();
        let attachment_buffer = gtk::TextBuffer::new(None);
        attachment_buffer.set_text(&memo_attachments_editor_text(&initial_draft.attachments));
        let attachment_view = gtk::TextView::builder()
            .buffer(&attachment_buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(120)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        attachment_view.set_monospace(true);
        attachment_view.set_tooltip_text(Some(&crate::i18n::gettext(
            "One URI or INLINE:<content> per line; contents are not executed",
        )));
        let attachment_picker = gtk::Button::with_label(&crate::i18n::gettext("Add URI/File"));
        let attachment_controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        let attachment_hint = gtk::Label::builder()
            .label(&crate::i18n::gettext("URI:… or INLINE:… (one line per attachment)"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["dim-label"])
            .build();
        attachment_controls.append(&attachment_hint);
        attachment_controls.append(&attachment_picker);
        let attachment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        attachment_box.append(&attachment_view);
        attachment_box.append(&attachment_controls);
        attachment_group.add(&attachment_box);
        content_box.append(&attachment_group);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();
        toolbar_view.set_content(Some(&scroll));
        dialog.set_content(Some(&toolbar_view));

        for entry in [
            title_entry.clone(),
            dtstart_entry.clone(),
            class_entry.clone(),
            status_entry.clone(),
            url_entry.clone(),
            categories_entry.clone(),
        ] {
            let dirty_for_entry = Rc::clone(&dirty);
            entry.connect_changed(move |_| dirty_for_entry.set(true));
        }
        let dirty_for_description = Rc::clone(&dirty);
        description_view
            .buffer()
            .connect_changed(move |_| dirty_for_description.set(true));
        let dirty_for_attachments = Rc::clone(&dirty);
        attachment_buffer.connect_changed(move |_| dirty_for_attachments.set(true));

        let attachment_loading_for_picker = Rc::clone(&attachment_loading);
        let attachment_buffer_for_picker = attachment_buffer.clone();
        attachment_picker.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            attachment_loading_for_picker.set(true);
            let file_dialog = gtk::FileDialog::builder()
                .title(&crate::i18n::gettext("Select memo attachments"))
                .modal(true)
                .build();
            let buffer_for_result = attachment_buffer_for_picker.clone();
            let loading_for_result = Rc::clone(&attachment_loading_for_picker);
            file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                if let Ok(files) = result {
                    let mut values = buffer_for_result
                        .text(
                            &buffer_for_result.start_iter(),
                            &buffer_for_result.end_iter(),
                            false,
                        )
                        .lines()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                    for index in 0..files.n_items() {
                        let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                            continue;
                        };
                        let uri = file.uri().to_string();
                        if !uri.is_empty() {
                            values.push(format!("URI:{uri}"));
                        }
                    }
                    buffer_for_result.set_text(&values.join("\n"));
                }
                loading_for_result.set(false);
            });
        });

        let dialog_weak_for_close = dialog.downgrade();
        let close_confirmed_for_close = Rc::clone(&close_confirmed);
        let close_prompt_open_for_close = Rc::clone(&close_prompt_open);
        let dirty_for_close = Rc::clone(&dirty);
        let operation_for_close = Rc::clone(&operation_in_progress);
        let request_generation_for_close = Rc::clone(&request_generation);
        let save_button_for_close = save_button.clone();
        dialog.connect_close_request(move |_| {
            if close_confirmed_for_close.replace(false) {
                return glib::Propagation::Proceed;
            }
            match memo_close_decision(
                dirty_for_close.get(),
                operation_for_close.get(),
                close_prompt_open_for_close.get(),
            ) {
                MemoCloseDecision::Allow => return glib::Propagation::Proceed,
                MemoCloseDecision::BlockSave => {
                    if let Some(dialog) = dialog_weak_for_close.upgrade() {
                        show_toast_for_widget(&dialog, &crate::i18n::gettext("Save in progress; please wait"));
                    }
                    return glib::Propagation::Stop;
                }
                MemoCloseDecision::KeepPrompt => return glib::Propagation::Stop,
                MemoCloseDecision::Prompt => close_prompt_open_for_close.set(true),
            }
            let unsaved_title = crate::i18n::gettext("Unsaved Memo Changes");
            let unsaved_desc = crate::i18n::gettext("The memo has changed since the last confirmed save.");
            let confirmation = adw::AlertDialog::new(
                Some(&unsaved_title),
                Some(&unsaved_desc),
            );
            let cancel_lbl = crate::i18n::gettext("Cancel");
            let save_lbl = crate::i18n::gettext("Save");
            let discard_lbl = crate::i18n::gettext("Discard");
            confirmation.add_responses(&[
                ("cancel", &cancel_lbl),
                ("save", &save_lbl),
                ("discard", &discard_lbl),
            ]);
            confirmation.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            let dialog_weak_for_response = dialog_weak_for_close.clone();
            let close_confirmed_for_response = Rc::clone(&close_confirmed_for_close);
            let close_prompt_open_for_response = Rc::clone(&close_prompt_open_for_close);
            let request_generation_for_response = Rc::clone(&request_generation_for_close);
            let save_button_for_response = save_button_for_close.clone();
            confirmation.connect_response(None, move |alert, response| {
                alert.close();
                close_prompt_open_for_response.set(false);
                let Some(dialog) = dialog_weak_for_response.upgrade() else {
                    return;
                };
                match response {
                    "discard" => {
                        request_generation_for_response
                            .set(request_generation_for_response.get().wrapping_add(1));
                        close_confirmed_for_response.set(true);
                        dialog.close();
                    }
                    "save" => save_button_for_response.emit_clicked(),
                    _ => {}
                }
            });
            if let Some(dialog) = dialog_weak_for_close.upgrade() {
                confirmation.present(Some(&dialog));
            } else {
                close_prompt_open_for_close.set(false);
            }
            glib::Propagation::Stop
        });
        let dialog_weak_for_cancel = dialog.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                dialog.close();
            }
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let save_button_for_load = save_button.clone();
        let local_calendar_available = CalendarStoreService::new().is_writable();
        let edit_source_uid = memo_to_edit
            .as_ref()
            .map(|memo| memo.source_uid.clone())
            .filter(|uid| !uid.trim().is_empty());
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_memo_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        // The source list is authoritative only after discovery; an edited
        // memo from a missing source must remain visible but never writable.
        save_button.set_sensitive(false);
        let edit_source_uid_for_load = edit_source_uid.clone();
        let preferred_source_uid_for_load = preferred_source_uid.clone();
        let page_weak_for_dialog_source_load = self.downgrade();
        let dialog_weak_for_dialog_source_load = dialog.downgrade();
        let (dialog_source_sender, dialog_source_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let discovery =
                ECalService::new().discover_source_capabilities(ECalComponentType::Memo);
            if let Err(error) = dialog_source_sender.send_blocking(discovery) {
                tracing::debug!(%error, "Memo dialog source discovery was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let Ok(discovery) = dialog_source_receiver.recv().await else {
                return;
            };
            let Some(dialog) = dialog_weak_for_dialog_source_load.upgrade() else {
                return;
            };
            if !dialog.is_visible() {
                return;
            }
            let (sources, warning, discovery_failed) = match discovery {
                Ok(discovery) => (
                    project_memo_source_capabilities(discovery.sources),
                    discovery.warning,
                    false,
                ),
                Err(error) => (Vec::new(), Some(error.to_string()), true),
            };
            if let Some(warning) = warning {
                tracing::warn!(%warning, "Could not reach EDS memo lists for memo dialog");
            }
            let mut available_sources = HashSet::new();
            let mut unavailable_sources = HashSet::new();
            let mut writable_sources = HashSet::new();
            let mut names = Vec::new();
            let mut uids = source_uids_for_load.borrow_mut();
            uids.clear();
            for source in sources {
                names.push(memo_source_projection_label(&source));
                if source.available {
                    available_sources.insert(source.uid.clone());
                } else {
                    unavailable_sources.insert(source.uid.clone());
                }
                if source.writable {
                    writable_sources.insert(source.uid.clone());
                }
                uids.push(source.uid);
            }
            let preserved_source_uid = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .filter(|uid| {
                    *uid != "local-calendar" && !uids.iter().any(|candidate| candidate == uid)
                });
            if let Some(uid) = preserved_source_uid {
                names.push(format!("{} · {uid}", crate::i18n::gettext("Unavailable source")));
                uids.push(uid.to_string());
                unavailable_sources.insert(uid.to_string());
            }
            if local_calendar_available {
                names.push(crate::i18n::gettext("Local Memo List"));
                uids.push("local-calendar".to_string());
                available_sources.insert("local-calendar".to_string());
                writable_sources.insert("local-calendar".to_string());
            }
            if names.is_empty() {
                names.push(if discovery_failed {
                    crate::i18n::gettext("Memo lists are unreachable")
                } else {
                    crate::i18n::gettext("No active memo list available")
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            if let Some(page) = page_weak_for_dialog_source_load.upgrade() {
                page.imp().memo_available_sources.replace(available_sources);
                page.imp()
                    .memo_unavailable_sources
                    .replace(unavailable_sources);
                page.imp().memo_writable_sources.replace(writable_sources);
            }
            let selected_index = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
                .or_else(|| (!uids.is_empty()).then_some(0));
            let selected_source_uid = selected_index.and_then(|index| uids.get(index).cloned());
            drop(uids);
            if let Some(index) = selected_index {
                source_row_for_load.set_selected(index as u32);
                let writable = page_weak_for_dialog_source_load
                    .upgrade()
                    .is_some_and(|page| {
                        selected_source_uid
                            .as_deref()
                            .is_some_and(|uid| memo_source_is_writable_for_page(&page, uid))
                    });
                save_button_for_load.set_sensitive(writable);
            } else {
                save_button_for_load.set_sensitive(false);
            }
        });

        let dialog_weak = dialog.downgrade();
        let page_weak = self.downgrade();
        let source_uids_for_save = Rc::clone(&source_uids);
        let draft_for_save_signal = Rc::clone(&draft_for_save);
        let operation_for_save = Rc::clone(&operation_in_progress);
        let request_generation_for_save = Rc::clone(&request_generation);
        let close_confirmed_for_save = Rc::clone(&close_confirmed);
        let attachment_loading_for_save = Rc::clone(&attachment_loading);
        let memo_to_edit_for_save = memo_to_edit.clone();
        let source_display_name_for_save = memo_to_edit
            .as_ref()
            .map(|memo| memo.source_display_name.clone());
        let save_button_for_signal = save_button.clone();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            if operation_for_save.get() {
                return;
            }
            let selected = source_row.selected() as usize;
            let source_uid = if let Some(memo) = memo_to_edit_for_save.as_ref() {
                memo.source_uid.clone()
            } else if let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() {
                source_uid
            } else {
                show_toast_for_widget(&dialog, &crate::i18n::gettext("No active memo list is selected"));
                return;
            };
            if source_uid.trim().is_empty() {
                show_toast_for_widget(&dialog, &crate::i18n::gettext("The selected memo list is invalid"));
                return;
            }
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !memo_source_is_writable_for_page(&page, &source_uid) {
                show_toast_for_widget(
                    &dialog,
                    &crate::i18n::gettext("The selected memo list is not writable or not available"),
                );
                return;
            }
            if let Some(error) = memo_attachment_save_error(attachment_loading_for_save.get()) {
                show_toast_for_widget(&dialog, &error);
                return;
            }
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let attachment_text = attachment_buffer.text(
                &attachment_buffer.start_iter(),
                &attachment_buffer.end_iter(),
                false,
            );
            let mut draft = draft_for_save_signal.borrow().clone();
            draft.summary = title_entry.text().to_string();
            draft.dtstart = optional_text(&dtstart_entry.text());
            draft.class = optional_text(&class_entry.text());
            draft.status = optional_text(&status_entry.text());
            draft.url = optional_text(&url_entry.text());
            draft.categories = parse_categories_input(&categories_entry.text());
            draft.description = description.to_string();
            draft.attachments =
                match parse_memo_attachment_editor_text(&attachment_text, &draft.attachments) {
                    Ok(attachments) => attachments,
                    Err(error) => {
                        show_toast_for_widget(&dialog, &error);
                        return;
                    }
                };
            alarm_editor.set_summary(&draft.summary);
            draft.alarms = alarm_editor.alarms();
            let mut component = match draft.apply_to_component() {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&dialog, &error);
                    return;
                }
            };
            let modified = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            component = crate::services::calendar_store_service::update_ical_property(
                &component,
                "LAST-MODIFIED",
                Some(&modified),
            );
            if let Some(existing) = memo_to_edit_for_save.as_ref() {
                crate::services::ecal_service::register_known_ecal_component(
                    ECalComponentType::Memo,
                    &existing.source_uid,
                    &existing.uid,
                    &existing.component,
                );
            }
            let operation_uid = memo_to_edit_for_save.as_ref().map(|memo| memo.uid.clone());
            let expected_uid = operation_uid.clone();
            let generation = request_generation_for_save.get().wrapping_add(1);
            request_generation_for_save.set(generation);
            operation_for_save.set(true);
            save_button.set_sensitive(false);
            let source_uid_for_model = source_uid.clone();
            let source_uid_for_selection = source_uid_for_model.clone();
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let service = ECalService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_component(
                        &source_uid,
                        ECalComponentType::Memo,
                        uid,
                        &component,
                    ),
                    None => {
                        service.create_component(&source_uid, ECalComponentType::Memo, &component)
                    }
                };
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Memo mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog_weak.clone();
            let page_weak_for_result = page_weak.clone();
            let memo_list_for_result = memo_list.clone();
            let memo_records_for_result = Rc::clone(&memo_records);
            let empty_state_for_result = empty_state.clone();
            let save_button_for_result = save_button.clone();
            let operation_for_result = Rc::clone(&operation_for_save);
            let request_generation_for_result = Rc::clone(&request_generation_for_save);
            let close_confirmed_for_result = Rc::clone(&close_confirmed_for_save);
            let draft_for_result = Rc::clone(&draft_for_save_signal);
            let source_display_name_for_result = source_display_name_for_save.clone();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await;
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    return;
                };
                if !memo_result_is_current(
                    request_generation_for_result.get(),
                    generation,
                    dialog.is_visible(),
                ) {
                    return;
                }
                operation_for_result.set(false);
                save_button_for_result.set_sensitive(true);
                match result {
                    Ok(Ok(result)) => {
                        let (component, confirmed_draft) = match
                            confirmed_memo_component(result.component)
                        {
                            Ok(confirmed) => confirmed,
                            Err(error) => {
                                show_toast_for_widget(
                                    &dialog,
                                    &crate::i18n::gettext("Confirmed memo component is missing or invalid: {}").replace("{}", &error),
                                );
                                return;
                            }
                        };
                        let uid = match confirmed_memo_uid(
                            expected_uid.as_deref(),
                            &result.uid,
                            &confirmed_draft.uid,
                        ) {
                            Ok(uid) => uid,
                            Err(error) => {
                                show_toast_for_widget(
                                    &dialog,
                                    &format!("{}: {error}", crate::i18n::gettext("Confirmed memo identity is invalid")),
                                );
                                return;
                            }
                        };
                        let source_display_name =
                            source_display_name_for_result.unwrap_or_else(|| {
                                if source_uid_for_model == "local-calendar" {
                                    crate::i18n::gettext("Local Memo List")
                                } else {
                                    crate::i18n::gettext("Synchronized memo store")
                                }
                            });
                        let memo = MemoRecord {
                            uid: uid.clone(),
                            source_uid: source_uid_for_model,
                            source_display_name,
                            component,
                            summary: confirmed_draft.summary.clone(),
                            description: confirmed_draft.description.clone(),
                            updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                        };
                        *draft_for_result.borrow_mut() = confirmed_draft;
                        if let Some(page) = page_weak_for_result.upgrade() {
                            let generation =
                                page.imp().memo_refresh_generation.get().wrapping_add(1);
                            page.imp().memo_refresh_generation.set(generation);
                        }
                        upsert_memo_row(
                            &memo_list_for_result,
                            Rc::clone(&memo_records_for_result),
                            memo,
                        );
                        select_memo_row(&memo_list_for_result, &source_uid_for_selection, &uid);
                        empty_state_for_result.set_visible(false);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            refresh_memo_category_model(&page, &memo_list_for_result);
                            let message = if editing {
                                format!("{} ({uid})", crate::i18n::gettext("Note updated"))
                            } else {
                                format!("{} ({uid})", crate::i18n::gettext("Note saved"))
                            };
                            page.show_toast(&message);
                        }
                        close_confirmed_for_result.set(true);
                        dialog.close();
                    }
                    Ok(Err(error)) => show_toast_for_widget(
                        &dialog,
                        &format!("{}: {error}", crate::i18n::gettext("Could not save note")),
                    ),
                    Err(error) => show_toast_for_widget(
                        &dialog,
                        &format!("{}: {error}", crate::i18n::gettext("Note save result missing")),
                    ),
                }
            });
        });

        dialog.present();
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Memo page result");
    }
}

fn replace_memo_snapshot(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    memos: Vec<MemoRecord>,
) {
    let selection = memo_selection_from_rows(memo_list);
    memo_list.unselect_all();
    while let Some(child) = memo_list.first_child() {
        memo_list.remove(&child);
    }
    memo_records.replace(memos);
    for memo in memo_records.borrow().iter() {
        append_memo_row(memo_list, memo);
    }
    restore_memo_selection_rows(memo_list, &selection);
}

fn upsert_memo_row(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    memo: MemoRecord,
) {
    let selection = memo_selection_from_rows(memo_list);
    let existing_index = memo_records
        .borrow()
        .iter()
        .position(|existing| existing.source_uid == memo.source_uid && existing.uid == memo.uid);
    if let Some(index) = existing_index {
        memo_records.borrow_mut().remove(index);
        remove_memo_list_row(memo_list, &memo.source_uid, &memo.uid);
    }
    memo_records.borrow_mut().push(memo.clone());
    append_memo_row(memo_list, &memo);
    restore_memo_selection_rows(memo_list, &selection);
}

fn select_memo_row(memo_list: &gtk::ListBox, source_uid: &str, uid: &str) -> bool {
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            row.data::<String>("memo-id")
                .is_some_and(|value| value.as_ref() == uid)
                && row
                    .data::<String>("memo-source-uid")
                    .is_some_and(|value| value.as_ref() == source_uid)
        };
        if matches {
            memo_list.select_row(Some(&row));
            return true;
        }
    }
    false
}

fn append_memo_row(memo_list: &gtk::ListBox, memo: &MemoRecord) {
    let row = gtk::ListBoxRow::new();
    unsafe {
        row.set_data("memo-id", memo.uid.clone());
        row.set_data("memo-source-uid", memo.source_uid.clone());
        row.set_data("memo-search", memo_search_text(memo));
        row.set_data("memo-summary", memo.summary.clone());
        row.set_data("memo-description", memo.description.clone());
        row.set_data("memo-component", memo.component.clone());
        row.set_data("memo-source-display", memo.source_display_name.clone());
        row.set_data("memo-categories", component_categories(&memo.component));
        row.set_data("memo-sort-updated", memo.updated.to_lowercase());
        row.set_data("memo-sort-title", memo.summary.to_lowercase());
        row.set_data("memo-sort-source", memo.source_display_name.to_lowercase());
    }
    let title = glib::markup_escape_text(&memo.summary);
    let subtitle = glib::markup_escape_text(&memo.updated);
    let action_row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(subtitle.as_str())
        .title_lines(1)
        .subtitle_lines(1)
        .use_markup(true)
        .build();
    if let Some(color) = CalendarCategoryStore::default().color_for_component(&memo.component) {
        let category_dot = gtk::Label::new(None);
        category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
        category_dot.set_tooltip_text(Some(&crate::i18n::gettext("Memo category")));
        action_row.add_prefix(&category_dot);
    }
    row.set_child(Some(&action_row));
    memo_list.append(&row);
}

fn memo_search_text(memo: &MemoRecord) -> String {
    format!(
        "{} {} {} {} {}",
        memo.summary, memo.description, memo.updated, memo.source_display_name, memo.component
    )
    .to_lowercase()
}

fn refresh_memo_category_model(page: &MemosPage, memo_list: &gtk::ListBox) {
    let Some(model) = page.imp().memo_category_model.borrow().as_ref().cloned() else {
        return;
    };
    let Some(selector) = page.imp().memo_category_selector.borrow().as_ref().cloned() else {
        return;
    };

    let mut categories = CalendarCategoryStore::default()
        .load()
        .into_iter()
        .map(|category| category.name)
        .collect::<Vec<_>>();
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let values = unsafe {
            row.data::<String>("memo-categories")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default()
        };
        categories.extend(
            values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    categories.sort_by_cached_key(|category| category.to_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let current_filter = page.imp().memo_category_filter.borrow().clone();
    if let Some(filter) = current_filter.as_deref()
        && !filter.eq_ignore_ascii_case("without")
        && !categories
            .iter()
            .any(|category| category.eq_ignore_ascii_case(filter))
    {
        categories.push(filter.to_string());
        categories.sort_by_cached_key(|category| category.to_lowercase());
    }

    let mut names = vec![
        crate::i18n::gettext("All Categories"),
        crate::i18n::gettext("Uncategorized"),
    ];
    names.extend(categories);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &name_refs);
    let selected = current_filter
        .as_deref()
        .map(|filter| {
            if filter.eq_ignore_ascii_case("without") {
                1
            } else {
                names
                    .iter()
                    .position(|category| category.eq_ignore_ascii_case(filter))
                    .unwrap_or(0) as u32
            }
        })
        .unwrap_or(0);
    if selector.selected() != selected {
        selector.set_selected(selected);
    }
}

fn memo_matches_filter(
    memo: &MemoRecord,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &str,
) -> bool {
    memo_matches_filter_with_mode(
        memo,
        source_filter,
        category_filter,
        query,
        MemoSearchMode::Any,
    )
}

fn memo_matches_filter_with_mode(
    memo: &MemoRecord,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &str,
    search_mode: MemoSearchMode,
) -> bool {
    let category = category_filter
        .map(MemoCategoryFilter::from_setting)
        .unwrap_or_default();
    MemoFilter::new(
        search_mode,
        query.to_string(),
        category,
        source_filter.map(ToOwned::to_owned),
    )
    .is_ok_and(|filter| filter.matches(memo))
}

fn apply_memo_list_filter(
    memo_list: &gtk::ListBox,
    source_filter: Option<String>,
    category_filter: Option<String>,
    query: String,
) {
    apply_memo_list_filter_with_mode(
        memo_list,
        source_filter,
        category_filter,
        query,
        MemoSearchMode::Any,
        HashSet::new(),
    );
}

fn apply_memo_list_filter_with_mode(
    memo_list: &gtk::ListBox,
    source_filter: Option<String>,
    category_filter: Option<String>,
    query: String,
    search_mode: MemoSearchMode,
    hidden_sources: HashSet<String>,
) {
    memo_list.set_filter_func(move |row| {
        let Some(memo) = memo_record_from_row(row) else {
            return false;
        };
        if hidden_sources.contains(&memo.source_uid) {
            return false;
        }
        memo_matches_filter_with_mode(
            &memo,
            source_filter.as_deref(),
            category_filter.as_deref(),
            &query,
            search_mode,
        )
    });
}

fn memo_record_from_row(row: &gtk::ListBoxRow) -> Option<MemoRecord> {
    unsafe {
        Some(MemoRecord {
            uid: row.data::<String>("memo-id")?.as_ref().clone(),
            source_uid: row.data::<String>("memo-source-uid")?.as_ref().clone(),
            source_display_name: row
                .data::<String>("memo-source-display")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            component: row.data::<String>("memo-component")?.as_ref().clone(),
            summary: row
                .data::<String>("memo-summary")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            description: row
                .data::<String>("memo-description")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            updated: row
                .data::<String>("memo-sort-updated")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
        })
    }
}

fn category_filter_option(value: &str) -> Option<String> {
    match MemoCategoryFilter::from_setting(value) {
        MemoCategoryFilter::Any => None,
        MemoCategoryFilter::Without => Some("without".to_string()),
        MemoCategoryFilter::Named(value) => Some(value),
    }
}

fn set_category_selector_for_filter(selector: &gtk::DropDown, value: &str) {
    let selected = match MemoCategoryFilter::from_setting(value) {
        MemoCategoryFilter::Any => 0,
        MemoCategoryFilter::Without => 1,
        MemoCategoryFilter::Named(_) => {
            let Some(model) = selector.model().and_downcast::<gtk::StringList>() else {
                return;
            };
            (0..model.n_items())
                .find(|index| {
                    model
                        .string(*index)
                        .is_some_and(|name| name.eq_ignore_ascii_case(value))
                })
                .unwrap_or(0)
        }
    };
    selector.set_selected(selected);
}

fn memo_search_mode_index(mode: MemoSearchMode) -> u32 {
    match mode {
        MemoSearchMode::Any => 0,
        MemoSearchMode::Summary => 1,
        MemoSearchMode::Description => 2,
        MemoSearchMode::Advanced => 3,
    }
}

fn memo_search_mode_from_index(index: u32) -> MemoSearchMode {
    match index {
        1 => MemoSearchMode::Summary,
        2 => MemoSearchMode::Description,
        3 => MemoSearchMode::Advanced,
        _ => MemoSearchMode::Any,
    }
}

fn refresh_memo_filter_view(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    empty_state: &gtk::Label,
) {
    let source_filter = page.imp().memo_source_filter.borrow().clone();
    let category_filter = page.imp().memo_category_filter.borrow().clone();
    let query = page.imp().memo_search_query.borrow().clone();
    let search_mode = page.imp().memo_search_mode.get();
    let hidden_sources = page.imp().hidden_memo_sources.borrow().clone();
    let category = category_filter
        .as_deref()
        .map(MemoCategoryFilter::from_setting)
        .unwrap_or_default();
    let filter = MemoFilter::new(search_mode, query.clone(), category, source_filter.clone());
    apply_memo_list_filter_with_mode(
        memo_list,
        source_filter.clone(),
        category_filter.clone(),
        query.clone(),
        search_mode,
        hidden_sources.clone(),
    );

    let filtered_memos: Vec<MemoRecord> = match filter {
        Err(error) => {
            empty_state.set_label(&crate::i18n::gettext("Invalid advanced search: {}").replace("{}", &error));
            empty_state.set_visible(true);
            Vec::new()
        }
        Ok(filter) => {
            let matches: Vec<MemoRecord> = filter
                .filter(&memo_records.borrow())
                .into_iter()
                .filter(|memo| !hidden_sources.contains(&memo.source_uid))
                .cloned()
                .collect();
            let has_records = !matches.is_empty();
            let empty_text = if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                crate::i18n::gettext("No synchronized memos loaded")
            } else {
                crate::i18n::gettext("No matching memos")
            };
            empty_state.set_label(&empty_text);
            empty_state.set_visible(!has_records);
            matches
        }
    };

    let mut sorted_memos = filtered_memos;
    let sort = page.imp().memo_sort.get();
    let descending = page.imp().memo_sort_descending.get();
    sorted_memos.sort_by(|a, b| {
        let cmp = match sort {
            MemoSort::Title => a.summary.to_lowercase().cmp(&b.summary.to_lowercase()),
            MemoSort::Source => a.source_display_name.to_lowercase().cmp(&b.source_display_name.to_lowercase()),
            MemoSort::Updated => a.updated.cmp(&b.updated),
        };
        if descending {
            cmp.reverse()
        } else {
            cmp
        }
    });

    let canvas_width = page
        .imp()
        .memo_table_canvas
        .borrow()
        .as_ref()
        .map(|c| c.width() as f64)
        .filter(|&w| w > 100.0)
        .unwrap_or(700.0);
    let layout = prepare_memo_table_layout(&sorted_memos, canvas_width);
    let total_h = layout.total_height;
    page.imp().memo_table_layout.replace(Some(layout));

    if let Some(canvas) = page.imp().memo_table_canvas.borrow().as_ref() {
        canvas.set_content_height(total_h.max(80.0) as i32);
        canvas.queue_draw();
    }
    if let Some(header) = page.imp().memo_table_header.borrow().as_ref() {
        header.queue_draw();
    }

    memo_list.invalidate_filter();
    restore_memo_selection_state(
        page,
        memo_list,
        memo_records,
        page.imp().memo_refresh_generation.get(),
    );
    sync_memo_surface_selection(page, memo_list);
    update_memo_surface_widgets_from_page(page, memo_list, memo_records);
}

fn restore_memo_selection_state(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    generation: u64,
) {
    let records = memo_records.borrow().clone();
    let mut selection = page.imp().memo_selection.borrow().clone();
    selection.restore_after_refresh(&records, generation);
    page.imp().memo_selection.replace(selection.clone());
    unsafe {
        memo_list.set_data("canvas-selection", selection.selected().to_vec());
        if let Some(focus) = selection.focus() {
            memo_list.set_data("canvas-focus", focus.clone());
        }
    }
    restore_memo_selection_rows(memo_list, &selection);
}

fn memo_selection_identities(memo_list: &gtk::ListBox) -> Vec<MemoIdentity> {
    if let Some(ids) = unsafe { memo_list.data::<Vec<MemoIdentity>>("canvas-selection") } {
        return unsafe { ids.as_ref().clone() };
    }
    memo_list
        .selected_rows()
        .iter()
        .filter_map(|row| unsafe {
            Some(MemoIdentity::new(
                row.data::<String>("memo-source-uid")?.as_ref().clone(),
                row.data::<String>("memo-id")?.as_ref().clone(),
            ))
        })
        .collect()
}

fn memo_selection_from_rows(memo_list: &gtk::ListBox) -> MemoSelection {
    let mut selection = MemoSelection::default();
    selection.set_selected(memo_selection_identities(memo_list));
    let focus = if let Some(focus) = unsafe { memo_list.data::<MemoIdentity>("canvas-focus") } {
        Some(unsafe { focus.as_ref().clone() })
    } else {
        memo_list.selected_row().and_then(|row| unsafe {
            Some(MemoIdentity::new(
                row.data::<String>("memo-source-uid")?.as_ref().clone(),
                row.data::<String>("memo-id")?.as_ref().clone(),
            ))
        })
    };
    selection.set_focus(focus);
    selection
}

fn selected_memo_records(
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) -> Vec<MemoRecord> {
    let identities = memo_selection_identities(memo_list);
    let records = memo_records.borrow();
    identities
        .iter()
        .filter_map(|identity| {
            records
                .iter()
                .find(|memo| MemoIdentity::from(*memo) == *identity)
                .cloned()
        })
        .collect()
}

fn selected_memo_records_checked(
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) -> Result<Vec<MemoRecord>, String> {
    let identities = memo_selection_identities(memo_list);
    let records = memo_records.borrow();
    if let Some(identity) = identities.iter().find(|identity| {
        records
            .iter()
            .filter(|record| MemoIdentity::from(*record) == **identity)
            .count()
            != 1
    }) {
        return Err(crate::i18n::gettext("The memo data does not contain a unique identity {source} / {uid}")
            .replace("{source}", &identity.source_uid)
            .replace("{uid}", &identity.uid));
    }
    drop(records);
    let selected = selected_memo_records(memo_list, memo_records);
    if selected.len() != identities.len() {
        return Err(crate::i18n::gettext("The memo selection is no longer up to date; please select again"));
    }
    if selected.is_empty() {
        return Err(crate::i18n::gettext("No memo selected"));
    }
    Ok(selected)
}

fn sync_memo_surface_selection(page: &MemosPage, memo_list: &gtk::ListBox) {
    let identities = memo_selection_identities(memo_list);
    let focus = if let Some(focus) = unsafe { memo_list.data::<MemoIdentity>("canvas-focus") } {
        Some(unsafe { focus.as_ref().clone() })
    } else {
        memo_list.selected_row().and_then(|row| unsafe {
            Some(MemoIdentity::new(
                row.data::<String>("memo-source-uid")?.as_ref().clone(),
                row.data::<String>("memo-id")?.as_ref().clone(),
            ))
        })
    };
    let mut selection = page.imp().memo_selection.borrow_mut();
    selection.set_selected(identities);
    selection.set_generation(page.imp().memo_refresh_generation.get());
    if focus.is_some() {
        selection.set_focus(focus);
    }
}

fn restore_memo_selection_rows(memo_list: &gtk::ListBox, selection: &MemoSelection) {
    if selection.is_empty() {
        memo_list.unselect_all();
        return;
    }
    memo_list.unselect_all();
    let focus = selection.focus().cloned();
    for identity in selection
        .selected()
        .iter()
        .filter(|identity| focus.as_ref() != Some(*identity))
    {
        let mut child = memo_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            if !row.is_visible() {
                continue;
            }
            let matches = unsafe {
                row.data::<String>("memo-id")
                    .is_some_and(|value| value.as_ref() == &identity.uid)
                    && row
                        .data::<String>("memo-source-uid")
                        .is_some_and(|value| value.as_ref() == &identity.source_uid)
            };
            if matches {
                memo_list.select_row(Some(&row));
                break;
            }
        }
    }
    if let Some(focus) = focus {
        let mut child = memo_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            if !row.is_visible() {
                continue;
            }
            let matches = unsafe {
                row.data::<String>("memo-id")
                    .is_some_and(|value| value.as_ref() == &focus.uid)
                    && row
                        .data::<String>("memo-source-uid")
                        .is_some_and(|value| value.as_ref() == &focus.source_uid)
            };
            if matches {
                memo_list.select_row(Some(&row));
                break;
            }
        }
    }
}

fn refresh_memo_preview(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) {
    let Some(label) = page.imp().memo_preview_label.borrow().as_ref().cloned() else {
        return;
    };
    let state = page.imp().memo_preview_state.borrow().clone();
    let identities = memo_selection_identities(memo_list);
    if !state.visible || identities.len() != 1 {
        label.set_markup(&format!("<i>{}</i>", &crate::i18n::gettext("No unique memo selection")));
        return;
    }
    let selected = memo_records.borrow();
    match preview_for_selection(&selected, &identities) {
        Ok(Some(preview)) => label.set_markup(&preview.markup),
        Ok(None) => label.set_markup(&format!("<i>{}</i>", &crate::i18n::gettext("No unique memo selection"))),
        Err(error) => label.set_markup(&format!(
            "<i>{}: {}</i>",
            &crate::i18n::gettext("Memo preview unavailable"),
            glib::markup_escape_text(&error)
        )),
    }
}

fn update_memo_surface_widgets(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    save_button: &gtk::Button,
    export_button: &gtk::Button,
    delete_button: &gtk::Button,
    open_button: &gtk::Button,
    forward_button: &gtk::Button,
    open_url_button: &gtk::Button,
    print_button: &gtk::Button,
    save_as_button: &gtk::Button,
    copy_button: &gtk::Button,
    cut_button: &gtk::Button,
    paste_button: &gtk::Button,
    select_all_button: &gtk::Button,
) {
    let selected = selected_memo_records_checked(memo_list, memo_records).unwrap_or_default();
    let action_state = MemoActionState::from_selection(&selected, |source_uid| {
        memo_source_is_writable_for_page(page, source_uid)
    });
    save_button
        .set_sensitive(action_state.single_selection && action_state.all_selected_sources_writable);
    export_button.set_sensitive(action_state.can_export);
    delete_button.set_sensitive(action_state.can_delete);
    open_button.set_sensitive(action_state.can_open);
    forward_button.set_sensitive(action_state.can_forward);
    open_url_button.set_sensitive(action_state.can_open_url);
    print_button.set_sensitive(action_state.can_print);
    save_as_button.set_sensitive(action_state.can_save_as);
    copy_button.set_sensitive(action_state.can_copy);
    cut_button.set_sensitive(action_state.can_cut);
    let destination = page
        .imp()
        .memo_source_filter
        .borrow()
        .clone()
        .unwrap_or_else(|| "local-calendar".to_string());
    paste_button.set_sensitive(memo_source_is_writable_for_page(page, &destination));
    select_all_button
        .set_sensitive(action_state.can_select_all && memo_list.first_child().is_some());
    refresh_memo_preview(page, memo_list, memo_records);
    if let Some(stack) = page.imp().memo_detail_stack.borrow().as_ref() {
        if selected.len() == 1 {
            stack.set_visible_child_name("content");
        } else {
            stack.set_visible_child_name("empty");
        }
    }
}

fn update_memo_surface_widgets_from_page(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) {
    let Some(buttons) = page.imp().memo_surface_buttons.borrow().as_ref().cloned() else {
        return;
    };
    if buttons.len() != 12 {
        return;
    }
    update_memo_surface_widgets(
        page,
        memo_list,
        memo_records,
        &buttons[0],
        &buttons[1],
        &buttons[2],
        &buttons[3],
        &buttons[4],
        &buttons[5],
        &buttons[6],
        &buttons[7],
        &buttons[8],
        &buttons[9],
        &buttons[10],
        &buttons[11],
    );
}

fn memo_record_from_component(
    source_uid: &str,
    source_display_name: &str,
    component: &str,
    expected_uid: Option<&str>,
) -> Result<MemoRecord, String> {
    let draft = MemoEditorDraft::parse(component)?;
    if draft.uid.trim().is_empty() {
        return Err(crate::i18n::gettext("Confirmed VJOURNAL component has no UID"));
    }
    if expected_uid.is_some_and(|expected_uid| draft.uid != expected_uid) {
        return Err(crate::i18n::gettext("Confirmed VJOURNAL component does not have the requested UID"));
    }
    Ok(MemoRecord {
        uid: draft.uid,
        source_uid: source_uid.to_string(),
        source_display_name: source_display_name.to_string(),
        component: component.to_string(),
        summary: draft.summary,
        description: draft.description,
        updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
    })
}

/// Execute a paste batch off the GTK thread.  Every create is confirmed by
/// the backend's UID and component before it becomes a model record.  If a
/// later item fails, already-created items are removed again so the UI never
/// presents a partial local success.
fn execute_memo_paste_batch(
    plan: Vec<MemoPasteItem>,
) -> Result<Vec<(MemoPasteItem, MemoRecord)>, String> {
    let batch = crate::services::ecal_service::ECalBatchGuard::new();
    let service = ECalService::new();
    let mut created = Vec::<(String, String)>::new();
    let mut confirmed = Vec::with_capacity(plan.len());
    let mut failure = None;

    for item in plan {
        let source_uid = item.identity.source_uid.clone();
        let expected_uid = item.identity.uid.clone();
        match service.create_component(&source_uid, ECalComponentType::Memo, &item.component) {
            Ok(mutation) => {
                if !mutation.uid.trim().is_empty() {
                    created.push((source_uid.clone(), mutation.uid.clone()));
                }
                if mutation.uid != expected_uid {
                    failure = Some(format!(
                        "{} / {}: {}",
                        source_uid,
                        expected_uid,
                        crate::i18n::gettext("Backend UID does not match")
                    ));
                    break;
                }
                let Some(component) = mutation.component else {
                    failure = Some(format!(
                        "{} / {}: {}",
                        source_uid,
                        expected_uid,
                        crate::i18n::gettext("Backend returned no confirmed VJOURNAL component")
                    ));
                    break;
                };
                match memo_record_from_component(
                    &source_uid,
                    "Zielquelle",
                    &component,
                    Some(&expected_uid),
                ) {
                    Ok(memo) => confirmed.push((item, memo)),
                    Err(error) => {
                        failure = Some(format!("{} / {}: {error}", source_uid, expected_uid));
                        break;
                    }
                }
            }
            Err(error) => {
                failure = Some(format!("{} / {}: {error}", source_uid, expected_uid));
                break;
            }
        }
    }

    let Some(failure) = failure else {
        return Ok(confirmed);
    };
    let rollback_errors = {
        let _replay = crate::services::ecal_service::ECalHistoryReplayGuard::new();
        rollback_created_memos(&service, &created)
    };
    batch.discard(ECalComponentType::Memo);
    if rollback_errors.is_empty() {
        Err(crate::i18n::gettext("Paste aborted atomically: {}").replace("{}", &failure))
    } else {
        Err(crate::i18n::gettext("Paste failed; rollback incomplete: {failure}; {errors}")
            .replace("{failure}", &failure)
            .replace("{errors}", &rollback_errors.join("; ")))
    }
}

fn rollback_created_memos(service: &ECalService, created: &[(String, String)]) -> Vec<String> {
    created
        .iter()
        .rev()
        .filter_map(|(source_uid, uid)| {
            match service.remove_component(source_uid, ECalComponentType::Memo, uid) {
                Ok(mutation) if mutation.uid == *uid => None,
                Ok(mutation) => Some(format!(
                    "Rollback {} / {} lieferte UID {}",
                    source_uid, uid, mutation.uid
                )),
                Err(error) => Some(format!("Rollback {} / {}: {error}", source_uid, uid)),
            }
        })
        .collect()
}

/// Delete a batch off the GTK thread and restore already-deleted components
/// when a later UID fails.  The returned identities are consumed only after
/// the whole operation has succeeded.
fn execute_memo_delete_batch(planned: Vec<MemoRecord>) -> Result<Vec<MemoIdentity>, String> {
    let batch = crate::services::ecal_service::ECalBatchGuard::new();
    let service = ECalService::new();
    let mut deleted = Vec::with_capacity(planned.len());
    let mut failure = None;

    for memo in planned {
        let identity = MemoIdentity::from(&memo);
        crate::services::ecal_service::register_known_ecal_component(
            ECalComponentType::Memo,
            &memo.source_uid,
            &memo.uid,
            &memo.component,
        );
        match service.remove_component(&memo.source_uid, ECalComponentType::Memo, &memo.uid) {
            Ok(mutation) if mutation.uid == memo.uid => deleted.push(memo),
            Ok(_mutation) => {
                failure = Some(format!(
                    "{} / {}: {}",
                    identity.source_uid,
                    identity.uid,
                    crate::i18n::gettext("Backend UID does not match")
                ));
                break;
            }
            Err(error) => {
                failure = Some(format!(
                    "{} / {}: {error}",
                    identity.source_uid, identity.uid
                ));
                break;
            }
        }
    }

    let Some(failure) = failure else {
        return Ok(deleted.iter().map(MemoIdentity::from).collect());
    };

    let mut rollback_errors = Vec::new();
    {
        let _replay = crate::services::ecal_service::ECalHistoryReplayGuard::new();
        for memo in deleted.iter().rev() {
            match service.create_component(&memo.source_uid, ECalComponentType::Memo, &memo.component) {
                Ok(mutation) if mutation.uid == memo.uid => {}
                Ok(mutation) => rollback_errors.push(format!(
                    "Rollback {} / {} lieferte UID {}",
                    memo.source_uid, memo.uid, mutation.uid
                )),
                Err(error) => rollback_errors.push(format!(
                    "Rollback {} / {}: {error}",
                    memo.source_uid, memo.uid
                )),
            }
        }
    }
    batch.discard(ECalComponentType::Memo);
    if rollback_errors.is_empty() {
        Err(crate::i18n::gettext("Delete aborted atomically: {}").replace("{}", &failure))
    } else {
        Err(crate::i18n::gettext("Delete failed; rollback incomplete: {failure}; {errors}")
            .replace("{failure}", &failure)
            .replace("{errors}", &rollback_errors.join("; ")))
    }
}

fn reload_memo_snapshot_async(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    empty_state: &gtk::Label,
) {
    let generation = page.imp().memo_refresh_generation.get().wrapping_add(1);
    page.imp().memo_refresh_generation.set(generation);
    let page_weak = page.downgrade();
    let memo_list = memo_list.clone();
    let memo_records = Rc::clone(memo_records);
    let empty_state = empty_state.clone();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = load_snapshot_prefer_eds();
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Memo recovery snapshot was dropped");
        }
    });
    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().memo_refresh_generation.get() != generation {
            return;
        }
        if let Some(warning) = result.warning {
            tracing::warn!(%warning, "Memo recovery snapshot loaded with a backend warning");
        }
        let memo_count = result.snapshot.memos.len();
        replace_memo_snapshot(&memo_list, Rc::clone(&memo_records), result.snapshot.memos);
        empty_state.set_visible(memo_count == 0);
        let empty_text = if memo_count == 0 {
            crate::i18n::gettext("No synchronized memos found")
        } else {
            String::new()
        };
        empty_state.set_label(&empty_text);
        refresh_memo_category_model(&page, &memo_list);
        refresh_memo_filter_view(&page, &memo_list, &memo_records, &empty_state);
    });
}

fn memo_source_is_writable_for_page(page: &MemosPage, source_uid: &str) -> bool {
    if source_uid.trim().is_empty() {
        return false;
    }
    if source_uid == "local-calendar" {
        return CalendarStoreService::new().is_writable();
    }
    if page
        .imp()
        .memo_unavailable_sources
        .borrow()
        .contains(source_uid)
    {
        return false;
    }
    // Remote mutating operations require the authoritative client read-only
    // capability.  An unknown, unavailable, or read-only source never falls
    // back to a guessed writable state.
    page.imp()
        .memo_available_sources
        .borrow()
        .contains(source_uid)
        && page
            .imp()
            .memo_writable_sources
            .borrow()
            .contains(source_uid)
}

fn compare_memo_rows(
    left: &gtk::ListBoxRow,
    right: &gtk::ListBoxRow,
    sort: MemoSort,
    descending: bool,
) -> std::cmp::Ordering {
    let data = |row: &gtk::ListBoxRow, key: &str| unsafe {
        row.data::<String>(key)
            .map(|value| value.as_ref().clone())
            .unwrap_or_default()
    };
    let ordering = match sort {
        MemoSort::Updated => data(left, "memo-sort-updated").cmp(&data(right, "memo-sort-updated")),
        MemoSort::Title => data(left, "memo-sort-title").cmp(&data(right, "memo-sort-title")),
        MemoSort::Source => data(left, "memo-sort-source").cmp(&data(right, "memo-sort-source")),
    };
    let ordering = if descending {
        ordering.reverse()
    } else {
        ordering
    };
    let source_order = data(left, "memo-source-uid").cmp(&data(right, "memo-source-uid"));
    let source_order = if descending {
        source_order.reverse()
    } else {
        source_order
    };
    let id_order = data(left, "memo-id").cmp(&data(right, "memo-id"));
    let id_order = if descending {
        id_order.reverse()
    } else {
        id_order
    };
    ordering.then(source_order).then(id_order)
}

fn remove_memo_list_row(memo_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("memo-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = row
                .data::<String>("memo-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            memo_list.remove(&row);
            return;
        }
    }
}

fn remove_memo_row(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    source_uid: &str,
    uid: &str,
) -> bool {
    let selection = memo_selection_from_rows(memo_list);
    let Some(index) = memo_records
        .borrow()
        .iter()
        .position(|memo| memo.source_uid == source_uid && memo.uid == uid)
    else {
        return false;
    };
    memo_records.borrow_mut().remove(index);
    remove_memo_list_row(memo_list, source_uid, uid);
    restore_memo_selection_rows(memo_list, &selection);
    true
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Memo UI result");
}

fn save_memo_file_async(
    button: &gtk::Button,
    file: gio::File,
    payload: String,
    success_message: impl Into<String>,
) {
    let button = button.clone();
    let success_message = success_message.into();
    file.replace_contents_async(
        payload.into_bytes(),
        None,
        true,
        gio::FileCreateFlags::NONE,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(_) => show_toast_for_widget(&button, &success_message),
            Err((_, error)) => show_toast_for_widget(
                &button,
                &crate::i18n::gettext("Could not save iCalendar: {}").replace("{}", &error.to_string()),
            ),
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoCloseDecision {
    Allow,
    Prompt,
    KeepPrompt,
    BlockSave,
}

fn memo_close_decision(dirty: bool, saving: bool, prompt_open: bool) -> MemoCloseDecision {
    if saving {
        MemoCloseDecision::BlockSave
    } else if !dirty {
        MemoCloseDecision::Allow
    } else if prompt_open {
        MemoCloseDecision::KeepPrompt
    } else {
        MemoCloseDecision::Prompt
    }
}

fn memo_result_is_current(
    current_generation: u64,
    expected_generation: u64,
    dialog_visible: bool,
) -> bool {
    current_generation == expected_generation && dialog_visible
}

fn confirmed_memo_component(
    component: Option<String>,
) -> Result<(String, MemoEditorDraft), String> {
    let component = component
        .filter(|component| !component.trim().is_empty())
        .ok_or_else(|| crate::i18n::gettext("ECal returned no complete component"))?;
    let draft = MemoEditorDraft::from_component(&component).map_err(|error| format!("{error}"))?;
    Ok((component, draft))
}

fn confirmed_memo_uid(
    expected_uid: Option<&str>,
    result_uid: &str,
    component_uid: &str,
) -> Result<String, String> {
    if let Some(expected_uid) = expected_uid {
        if component_uid != expected_uid {
            return Err(crate::i18n::gettext("Component UID does not match the request"));
        }
        return Ok(expected_uid.to_string());
    }
    if !result_uid.trim().is_empty() && result_uid != component_uid {
        return Err(crate::i18n::gettext("ECal result and confirmed component have different UIDs"));
    }
    let uid = if result_uid.trim().is_empty() {
        component_uid
    } else {
        result_uid
    };
    (!uid.trim().is_empty())
        .then(|| uid.to_string())
        .ok_or_else(|| crate::i18n::gettext("The confirmed memo component has no UID"))
}

#[cfg(test)]
fn memo_delete_confirmed(response: &str) -> bool {
    response == "confirm"
}

fn memo_attachment_save_error(loading: bool) -> Option<String> {
    loading.then(|| crate::i18n::gettext("Attachments are still loading; please wait"))
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn serialize_categories_vec(categories: &[String]) -> String {
    categories
        .iter()
        .map(|category| escape_ical_text(category.trim()))
        .filter(|category| !category.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_categories_input(value: &str) -> Vec<String> {
    let mut categories = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            if !current.trim().is_empty() {
                categories.push(unescape_ical_text(current.trim()));
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        categories.push(unescape_ical_text(current.trim()));
    }
    categories
}

fn memo_attachment_editor_line(attachment: &MemoAttachment) -> String {
    if attachment.is_uri() && !attachment.is_inline() {
        let uri = attachment.uri.as_deref().unwrap_or_default();
        format!("URI:{uri}")
    } else {
        format!(
            "INLINE:{}",
            attachment.inline_content.as_deref().unwrap_or_default()
        )
    }
}

fn memo_attachments_editor_text(attachments: &[MemoAttachment]) -> String {
    attachments
        .iter()
        .map(memo_attachment_editor_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn attachment_with_editor_value(original: &MemoAttachment, line: &str) -> Option<MemoAttachment> {
    let mut attachment = original.clone();
    if let Some(uri) = line.strip_prefix("URI:") {
        let uri = uri.trim();
        if uri.is_empty() {
            return None;
        }
        attachment.uri = Some(uri.to_string());
        attachment.inline_content = None;
    } else if let Some(content) = line.strip_prefix("INLINE:") {
        attachment.uri = None;
        attachment.inline_content = Some(content.to_string());
    } else {
        let uri = line.trim();
        if uri.is_empty() {
            return None;
        }
        attachment.uri = Some(uri.to_string());
        attachment.inline_content = None;
    }
    Some(attachment)
}

fn parse_memo_attachment_editor_text(
    value: &str,
    existing: &[MemoAttachment],
) -> Result<Vec<MemoAttachment>, String> {
    let mut used_existing = vec![false; existing.len()];
    let mut attachments = Vec::new();
    for (line_index, raw_line) in value.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains('\r') || line.contains('\n') {
            return Err(crate::i18n::gettext("The attachment list contains invalid line breaks"));
        }
        if let Some((index, attachment)) =
            existing.iter().enumerate().find(|(index, attachment)| {
                !used_existing[*index] && memo_attachment_editor_line(attachment) == line
            })
        {
            used_existing[index] = true;
            attachments.push(attachment.clone());
            continue;
        }
        if let Some((index, attachment)) = existing
            .get(line_index)
            .filter(|_| !used_existing.get(line_index).copied().unwrap_or(true))
            .map(|attachment| (line_index, attachment))
        {
            if let Some(attachment) = attachment_with_editor_value(attachment, line) {
                used_existing[index] = true;
                attachments.push(attachment);
                continue;
            }
        }
        if let Some(uri) = line.strip_prefix("URI:") {
            let uri = uri.trim();
            if uri.is_empty() {
                return Err(crate::i18n::gettext("A URI attachment cannot be empty"));
            }
            attachments.push(MemoAttachment::uri(uri.to_string()));
        } else if let Some(content) = line.strip_prefix("INLINE:") {
            attachments.push(MemoAttachment::inline(content.to_string()));
        } else {
            // A bare line is accepted as a URI for the same compact workflow
            // as Evolution's attachment page.  It is only serialized; it is
            // never opened or fetched by this editor.
            attachments.push(MemoAttachment::uri(line.to_string()));
        }
    }
    Ok(attachments)
}

fn build_memo_component(
    title: &str,
    description: &str,
    categories: &str,
    alarms: &[Alarm],
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(crate::i18n::gettext("A memo requires a title"));
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut component = vec![
        "BEGIN:VJOURNAL".to_string(),
        format!("UID:meridian-{}", uuid::Uuid::new_v4()),
        format!("DTSTAMP:{stamp}"),
        format!("DTSTART:{stamp}"),
        format!("SUMMARY:{}", escape_ical_text(title)),
    ];
    if !description.trim().is_empty() {
        component.push(format!(
            "DESCRIPTION:{}",
            escape_ical_text(description.trim())
        ));
    }
    let categories = serialize_categories(categories);
    if !categories.is_empty() {
        component.push(format!("CATEGORIES:{categories}"));
    }
    component.push("END:VJOURNAL".to_string());
    Ok(replace_component_alarms(
        &format!("{}\r\n", component.join("\r\n")),
        alarms,
    ))
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn serialize_categories(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .map(escape_ical_text)
        .collect::<Vec<_>>()
        .join(",")
}

fn component_categories(component: &str) -> String {
    component
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .find_map(|line| {
            let (property, value) = line.split_once(':')?;
            let name = property.split(';').next().unwrap_or(property).trim();
            (name.eq_ignore_ascii_case("CATEGORIES")).then(|| unescape_ical_text(value))
        })
        .unwrap_or_default()
}

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::calendar_store_service::MemoAttachmentParameter;

    #[test]
    fn memo_component_serialization_is_ical_safe() {
        let component = build_memo_component(
            "A, note; title",
            "Line one\nLine two",
            "Work, Personal",
            &[],
        )
        .expect("memo component should be valid");
        assert!(component.starts_with("BEGIN:VJOURNAL\r\n"));
        assert!(component.contains("SUMMARY:A\\, note\\; title\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.contains("CATEGORIES:Work,Personal\r\n"));
        assert!(component.ends_with("END:VJOURNAL\r\n"));
    }

    #[test]
    fn memo_component_rejects_empty_title() {
        assert!(build_memo_component("  ", "body", "", &[]).is_err());
    }

    #[test]
    fn memo_categories_roundtrip_through_component_text() {
        let component = "BEGIN:VJOURNAL\r\nCATEGORIES:Work\\, urgent,Personal\r\nEND:VJOURNAL\r\n";
        assert_eq!(component_categories(component), "Work, urgent,Personal");
        assert_eq!(serialize_categories("Work, Personal"), "Work,Personal");
    }

    #[test]
    fn memo_source_capability_projection_is_exact_sorted_and_fail_closed() {
        let sources = project_memo_source_capabilities(vec![
            ECalSourceCapability {
                source_uid: "readonly".to_string(),
                display_name: "Zulu".to_string(),
                connected: true,
                available: true,
                writable: false,
                source_writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "offline".to_string(),
                display_name: "Alpha".to_string(),
                connected: false,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "writable".to_string(),
                display_name: "Beta".to_string(),
                connected: true,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "writable".to_string(),
                display_name: "Duplicate must not win".to_string(),
                connected: true,
                available: true,
                writable: false,
                source_writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "  ".to_string(),
                display_name: "Invalid".to_string(),
                connected: true,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
        ]);

        assert_eq!(
            sources
                .iter()
                .map(|source| source.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["offline", "writable", "readonly"]
        );
        assert!(!sources[0].available);
        assert!(!sources[0].writable);
        assert_eq!(
            memo_source_projection_label(&sources[0]),
            format!("{} · Alpha", crate::i18n::gettext("Unreachable"))
        );
        assert!(sources[1].available);
        assert!(sources[1].writable);
        assert!(!sources[2].writable);
        assert_eq!(
            memo_source_projection_label(&sources[2]),
            format!("{} · Zulu", crate::i18n::gettext("Read-Only"))
        );
    }

    #[test]
    fn memo_search_text_includes_title_description_and_component() {
        let memo = MemoRecord {
            uid: "memo-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Lokal".to_string(),
            component: "CATEGORIES:Project-X".to_string(),
            summary: "Release notes".to_string(),
            description: "Remember the migration".to_string(),
            updated: "2026-08-11 10:00".to_string(),
        };
        let search = memo_search_text(&memo);
        assert!(search.contains("release notes"));
        assert!(search.contains("migration"));
        assert!(search.contains("project-x"));
        assert!(memo_matches_filter(&memo, None, Some("project-x"), ""));
        assert!(!memo_matches_filter(&memo, None, Some("personal"), ""));
    }

    #[test]
    fn memo_attachment_editor_keeps_existing_uri_and_inline_values_in_order() {
        let mut first = MemoAttachment::uri("https://example.test/a");
        first.filename = Some("a.txt".to_string());
        first.fmttype = Some("text/plain".to_string());
        first.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-KEEP".to_string(),
            value: Some("yes".to_string()),
        });
        let second = MemoAttachment::inline("UE5H");
        let existing = vec![first.clone(), second.clone()];
        let text = memo_attachments_editor_text(&existing);
        let parsed = parse_memo_attachment_editor_text(&text, &existing)
            .expect("unchanged attachment editor text should parse");
        assert_eq!(parsed, existing);

        let changed = parse_memo_attachment_editor_text(
            "URI:https://example.test/a\nINLINE:UE5H\nURI:https://example.test/c",
            &existing,
        )
        .expect("new URI should be accepted");
        assert_eq!(changed.len(), 3);
        assert_eq!(changed[0].filename.as_deref(), Some("a.txt"));
        assert_eq!(changed[1].inline_content.as_deref(), Some("UE5H"));
        assert_eq!(changed[2].uri.as_deref(), Some("https://example.test/c"));
    }

    #[test]
    fn memo_attachment_editor_changes_retain_existing_metadata() {
        let mut uri = MemoAttachment::uri("https://example.test/old");
        uri.filename = Some("memo.txt".to_string());
        uri.fmttype = Some("text/plain".to_string());
        uri.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-SAFE".to_string(),
            value: Some("keep".to_string()),
        });
        let mut inline = MemoAttachment::inline("old-payload");
        inline.filename = Some("payload.bin".to_string());
        inline.fmttype = Some("application/octet-stream".to_string());
        inline.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-INLINE".to_string(),
            value: Some("keep".to_string()),
        });
        let existing = vec![uri, inline];
        let changed = parse_memo_attachment_editor_text(
            "URI:https://example.test/new\nINLINE:new-payload",
            &existing,
        )
        .expect("edited attachment values should parse");
        assert_eq!(changed[0].uri.as_deref(), Some("https://example.test/new"));
        assert_eq!(changed[0].filename.as_deref(), Some("memo.txt"));
        assert_eq!(changed[0].fmttype.as_deref(), Some("text/plain"));
        assert_eq!(changed[0].unknown_parameters[0].name, "X-SAFE");
        assert_eq!(changed[1].inline_content.as_deref(), Some("new-payload"));
        assert_eq!(changed[1].filename.as_deref(), Some("payload.bin"));
        assert_eq!(changed[1].unknown_parameters[0].name, "X-INLINE");
    }

    #[test]
    fn memo_dialog_close_and_mutation_guards_cover_cancel_dirty_and_stale_results() {
        assert_eq!(
            memo_close_decision(false, false, false),
            MemoCloseDecision::Allow
        );
        assert_eq!(
            memo_close_decision(true, false, false),
            MemoCloseDecision::Prompt
        );
        assert_eq!(
            memo_close_decision(true, false, true),
            MemoCloseDecision::KeepPrompt
        );
        assert_eq!(
            memo_close_decision(true, true, false),
            MemoCloseDecision::BlockSave
        );
        assert!(!memo_delete_confirmed("cancel"));
        assert!(memo_delete_confirmed("confirm"));
        assert!(memo_result_is_current(4, 4, true));
        assert!(!memo_result_is_current(4, 3, true));
        assert!(!memo_result_is_current(4, 4, false));
    }

    #[test]
    fn invalid_and_loading_attachment_states_are_rejected_before_component_apply() {
        let existing = Vec::new();
        assert!(parse_memo_attachment_editor_text("URI:", &existing).is_err());
        let save_err = memo_attachment_save_error(true);
        assert!(
            save_err == Some("Attachments are still loading; please wait".to_string())
                || save_err == Some("Anhänge werden noch geladen; bitte warten".to_string())
        );
        assert_eq!(memo_attachment_save_error(false), None);
        let mut draft = MemoEditorDraft::from_component(
            "BEGIN:VJOURNAL\r\nUID:memo-invalid\r\nSUMMARY:Valid\r\nEND:VJOURNAL\r\n",
        )
        .expect("valid draft should parse");
        let mut invalid_attachment = MemoAttachment::uri("https://example.test/a\nunsafe");
        invalid_attachment.filename = Some("bad\nname".to_string());
        draft.attachments = vec![invalid_attachment];
        assert!(draft.apply().is_err());
    }

    #[test]
    fn confirmed_memo_result_is_required_before_model_data_can_be_built() {
        assert!(confirmed_memo_component(None).is_err());
        assert!(confirmed_memo_component(Some(String::new())).is_err());
        assert!(
            confirmed_memo_component(Some(
                "BEGIN:VEVENT\r\nUID:not-a-memo\r\nEND:VEVENT\r\n".to_string()
            ))
            .is_err()
        );
        let component = "BEGIN:VJOURNAL\r\nUID:confirmed\r\nSUMMARY:Confirmed\r\nEND:VJOURNAL\r\n";
        let (confirmed, draft) = confirmed_memo_component(Some(component.to_string()))
            .expect("complete confirmed memo should be accepted");
        assert_eq!(confirmed, component);
        assert_eq!(draft.uid, "confirmed");
        assert_eq!(draft.summary, "Confirmed");
        assert!(confirmed_memo_uid(Some("expected"), "expected", "other").is_err());
        assert!(confirmed_memo_uid(None, "one", "two").is_err());
        assert!(memo_record_from_component("source", "Source", component, Some("other")).is_err());
        assert!(
            memo_record_from_component("source", "Source", component, Some("confirmed")).is_ok()
        );
        assert_eq!(
            confirmed_memo_uid(None, "", "component").expect("component UID is usable"),
            "component"
        );
    }

    #[test]
    fn test_memo_table_canvas_layout_and_hit_testing() {
        let memos = vec![
            MemoRecord {
                uid: "memo-1".to_string(),
                source_uid: "source-a".to_string(),
                source_display_name: "Personal".to_string(),
                component: "BEGIN:VJOURNAL\r\nSUMMARY:First Note\r\nCATEGORIES:Work\r\nEND:VJOURNAL\r\n".to_string(),
                summary: "First Note".to_string(),
                description: "Description 1".to_string(),
                updated: "2026-09-15T12:00:00Z".to_string(),
            },
            MemoRecord {
                uid: "memo-2".to_string(),
                source_uid: "source-b".to_string(),
                source_display_name: "Work".to_string(),
                component: "BEGIN:VJOURNAL\r\nSUMMARY:Second Note\r\nEND:VJOURNAL\r\n".to_string(),
                summary: "Second Note".to_string(),
                description: "Description 2".to_string(),
                updated: "2026-09-15T13:00:00Z".to_string(),
            },
        ];

        let layout = prepare_memo_table_layout(&memos, 800.0);
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(layout.rows[0].identity.uid, "memo-1");
        assert_eq!(layout.rows[1].identity.uid, "memo-2");
        assert_eq!(layout.rows[0].categories, vec!["Work"]);
        assert!(layout.rows[1].categories.is_empty());

        // Hit testing rows
        assert_eq!(hit_test_memo_table(&layout, 10.0), Some(0));
        assert_eq!(hit_test_memo_table(&layout, 35.0), Some(1));
        assert_eq!(hit_test_memo_table(&layout, 150.0), None);

        // Hit testing header columns
        let header_title_x = layout.col_color_w + 10.0;
        let header_source_x = layout.col_color_w + layout.col_title_w + 10.0;
        let header_updated_x = layout.col_color_w + layout.col_title_w + layout.col_source_w + 10.0;
        let header_categories_x = layout.col_color_w + layout.col_title_w + layout.col_source_w + layout.col_updated_w + 10.0;

        assert_eq!(hit_test_memo_table_header(&layout, -5.0), None);
        assert_eq!(hit_test_memo_table_header(&layout, 5.0), Some(MemoTableColumn::Color));
        assert_eq!(hit_test_memo_table_header(&layout, header_title_x), Some(MemoTableColumn::Title));
        assert_eq!(hit_test_memo_table_header(&layout, header_source_x), Some(MemoTableColumn::Source));
        assert_eq!(hit_test_memo_table_header(&layout, header_updated_x), Some(MemoTableColumn::Updated));
        assert_eq!(hit_test_memo_table_header(&layout, header_categories_x), Some(MemoTableColumn::Categories));
    }
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
