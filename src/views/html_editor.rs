use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// HtmlEditor — WebKitGTK 6.0 based WYSIWYG HTML editor for mail composition.
//
// Features:
// - Contenteditable HTML editor canvas
// - Formatting toolbar (Bold, Italic, Underline, Lists, Blockquote, Links)
// - Sandboxed DOM manipulation via JavaScript

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use webkit6::prelude::*;

use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;

pub const COMPOSER_FONT_FAMILIES: &[(&str, &str)] = &[
    ("System Default", "system-ui, sans-serif"),
    ("Sans-Serif", "sans-serif"),
    ("Serif", "serif"),
    ("Monospace", "monospace"),
    ("Cantarell", "Cantarell, sans-serif"),
    ("Inter", "Inter, sans-serif"),
    ("Roboto", "Roboto, sans-serif"),
    ("Arial", "Arial, Helvetica, sans-serif"),
    ("Verdana", "Verdana, Geneva, sans-serif"),
    ("Tahoma", "Tahoma, Geneva, sans-serif"),
    ("Times New Roman", "'Times New Roman', Times, serif"),
    ("Georgia", "Georgia, serif"),
    ("Courier New", "'Courier New', Courier, monospace"),
];

pub fn font_family_index_for_css(raw: &str) -> u32 {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() || lower.contains("system-ui") {
        return 0;
    }
    if lower.contains("inter") {
        return 5;
    }
    if lower.contains("roboto") {
        return 6;
    }
    if lower.contains("arial") || lower.contains("helvetica") {
        return 7;
    }
    if lower.contains("verdana") {
        return 8;
    }
    if lower.contains("tahoma") {
        return 9;
    }
    if lower.contains("times") {
        return 10;
    }
    if lower.contains("georgia") {
        return 11;
    }
    if lower.contains("courier") {
        return 12;
    }
    if lower.contains("cantarell") {
        return 4;
    }
    if lower == "monospace" || lower.ends_with("monospace") {
        return 3;
    }
    if lower == "serif" || (lower.ends_with("serif") && !lower.contains("sans-serif")) {
        return 2;
    }
    if lower == "sans-serif" {
        return 1;
    }
    0
}

fn primary_webkit_font_family(css_family: &str, desktop_font: &str) -> String {
    let idx = font_family_index_for_css(css_family);
    match idx {
        4 => "Cantarell".to_string(),
        5 => "Inter".to_string(),
        6 => "Roboto".to_string(),
        7 => "Arial".to_string(),
        8 => "Verdana".to_string(),
        9 => "Tahoma".to_string(),
        10 => "Times New Roman".to_string(),
        11 => "Georgia".to_string(),
        12 => "Courier New".to_string(),
        _ => desktop_font.to_string(),
    }
}

pub const COMPOSER_FONT_SIZES: &[u32] = &[
    8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 24, 28, 32, 36, 48,
];

pub fn font_size_index_for_px(px: u32) -> u32 {
    let target = if px == 0 { 14 } else { px };
    if let Some(pos) = COMPOSER_FONT_SIZES.iter().position(|&s| s == target) {
        return pos as u32;
    }
    if let Some((best_pos, _)) = COMPOSER_FONT_SIZES
        .iter()
        .enumerate()
        .min_by_key(|&(_, &s)| (s as i32 - target as i32).abs())
    {
        return best_pos as u32;
    }
    6
}

pub fn font_size_px_for_index(idx: u32) -> u32 {
    COMPOSER_FONT_SIZES
        .get(idx as usize)
        .copied()
        .unwrap_or(14)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomBlockStyle {
    #[serde(default)]
    pub font_family: String,
    #[serde(default)]
    pub font_size_px: u32,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    #[serde(default)]
    pub color: String,
}

impl CustomBlockStyle {
    pub fn to_inline_css(&self) -> String {
        let mut rules = Vec::new();
        if !self.font_family.trim().is_empty() {
            let safe_ff = crate::views::mail_viewer::safe_css_font_family(&self.font_family);
            rules.push(format!("font-family: {safe_ff};"));
        }
        if self.font_size_px >= 8 && self.font_size_px <= 72 {
            rules.push(format!("font-size: {}px;", self.font_size_px));
        }
        if let Some(bold) = self.bold {
            rules.push(format!(
                "font-weight: {};",
                if bold { "bold" } else { "normal" }
            ));
        }
        if let Some(italic) = self.italic {
            rules.push(format!(
                "font-style: {};",
                if italic { "italic" } else { "normal" }
            ));
        }
        if let Some(underline) = self.underline {
            rules.push(format!(
                "text-decoration: {};",
                if underline { "underline" } else { "none" }
            ));
        }
        let col = self.color.trim();
        if !col.is_empty()
            && !col.contains(';')
            && !col.contains('{')
            && !col.contains('}')
            && !col.contains('<')
        {
            rules.push(format!("color: {col};"));
        }
        rules.join(" ")
    }
}

fn custom_composer_styles_path() -> std::path::PathBuf {
    glib::user_config_dir()
        .join("meridian")
        .join("composer_styles.json")
}

pub fn load_custom_composer_styles() -> std::collections::BTreeMap<String, CustomBlockStyle> {
    let path = custom_composer_styles_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_custom_composer_styles(
    styles: &std::collections::BTreeMap<String, CustomBlockStyle>,
) {
    let path = custom_composer_styles_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(styles) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn build_custom_styles_css(
    styles: &std::collections::BTreeMap<String, CustomBlockStyle>,
) -> String {
    let mut css = String::new();
    for (tag, style) in styles {
        if !matches!(tag.as_str(), "p" | "h1" | "h2" | "h3" | "pre") {
            continue;
        }
        let inline = style.to_inline_css();
        if !inline.is_empty() {
            css.push_str(&format!("{tag} {{ {inline} }}\n"));
        }
    }
    css
}

pub const MAX_RECENT_COMPOSER_COLORS: usize = 5;

fn recent_composer_colors_path() -> std::path::PathBuf {
    glib::user_config_dir()
        .join("meridian")
        .join("composer_recent_colors.json")
}

pub fn load_recent_composer_colors() -> Vec<String> {
    let path = recent_composer_colors_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

pub fn save_recent_composer_colors(colors: &[String]) {
    let path = recent_composer_colors_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(colors) {
        let _ = std::fs::write(&path, json);
    }
}

fn draw_cairo_rounded_rect(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
) {
    let deg = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90.0 * deg, 0.0 * deg);
    cr.arc(x + w - r, y + h - r, r, 0.0 * deg, 90.0 * deg);
    cr.arc(x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
    cr.arc(x + r, y + r, r, 180.0 * deg, 270.0 * deg);
    cr.close_path();
}

const SWATCH_BUTTON_SIZE: i32 = 24;
const SWATCH_AREA_SIZE: i32 = 24;
const SWATCH_OUTER_RADIUS: f64 = 10.0;

fn build_cairo_color_swatch_button(color_css: &str, tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::builder()
        .tooltip_text(tooltip)
        .width_request(SWATCH_BUTTON_SIZE)
        .height_request(SWATCH_BUTTON_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "composer-color-swatch"])
        .build();
    set_accessible_label(&btn, tooltip);

    let rgba = gtk::gdk::RGBA::parse(color_css)
        .unwrap_or_else(|_| gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
    let area = gtk::DrawingArea::builder()
        .content_width(SWATCH_AREA_SIZE)
        .content_height(SWATCH_AREA_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |widget, cr, w, h| {
        let cx = (w as f64) / 2.0;
        let cy = (h as f64) / 2.0;
        let selected = widget
            .parent()
            .is_some_and(|p| p.has_css_class("selected-swatch"));

        if selected {
            // FolderPropertiesDialog style: outer 2px accent ring + 1px inner gap + solid fill
            cr.arc(cx, cy, SWATCH_OUTER_RADIUS - 2.3, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_source_rgba(
                rgba.red() as f64,
                rgba.green() as f64,
                rgba.blue() as f64,
                rgba.alpha().max(0.88) as f64,
            );
            let _ = cr.fill();

            cr.arc(cx, cy, SWATCH_OUTER_RADIUS - 0.9, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_line_width(1.8);
            cr.set_source_rgba(0.208, 0.518, 0.894, 1.0);
            let _ = cr.stroke();
        } else {
            // Flat, borderless solid circle matching FolderPropertiesDialog
            cr.arc(cx, cy, SWATCH_OUTER_RADIUS, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_source_rgba(
                rgba.red() as f64,
                rgba.green() as f64,
                rgba.blue() as f64,
                rgba.alpha().max(0.88) as f64,
            );
            let _ = cr.fill();
        }
    });
    btn.set_child(Some(&area));
    btn
}

fn build_cairo_reset_swatch_button(tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::builder()
        .tooltip_text(tooltip)
        .width_request(SWATCH_BUTTON_SIZE)
        .height_request(SWATCH_BUTTON_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "composer-color-swatch", "selected-swatch"])
        .build();
    set_accessible_label(&btn, tooltip);

    let area = gtk::DrawingArea::builder()
        .content_width(SWATCH_AREA_SIZE)
        .content_height(SWATCH_AREA_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |widget, cr, w, h| {
        let cx = (w as f64) / 2.0;
        let cy = (h as f64) / 2.0;
        let selected = widget
            .parent()
            .is_some_and(|p| p.has_css_class("selected-swatch"));

        if selected {
            let stroke_w = 1.8;
            cr.arc(
                cx,
                cy,
                SWATCH_OUTER_RADIUS - stroke_w / 2.0,
                0.0,
                2.0 * std::f64::consts::PI,
            );
            cr.set_line_width(stroke_w);
            cr.set_source_rgba(0.208, 0.518, 0.894, 1.0);
            let _ = cr.stroke();
        } else {
            let stroke_w = 1.3;
            cr.arc(
                cx,
                cy,
                SWATCH_OUTER_RADIUS - stroke_w / 2.0,
                0.0,
                2.0 * std::f64::consts::PI,
            );
            cr.set_line_width(stroke_w);
            cr.set_source_rgba(0.5, 0.5, 0.5, 0.42);
            let _ = cr.stroke();
        }

        // Crisp "×" icon matching FolderPropertiesDialog's window-close-symbolic
        let d = 3.1;
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.move_to(cx - d, cy - d);
        cr.line_to(cx + d, cy + d);
        cr.move_to(cx + d, cy - d);
        cr.line_to(cx - d, cy + d);
        cr.set_line_width(1.6);
        cr.set_source_rgba(0.24, 0.22, 0.27, 0.90);
        let _ = cr.stroke();
    });
    btn.set_child(Some(&area));
    btn
}

fn build_cairo_custom_swatch_button(tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::builder()
        .tooltip_text(tooltip)
        .width_request(SWATCH_BUTTON_SIZE)
        .height_request(SWATCH_BUTTON_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "composer-color-swatch"])
        .build();
    set_accessible_label(&btn, tooltip);

    let area = gtk::DrawingArea::builder()
        .content_width(SWATCH_AREA_SIZE)
        .content_height(SWATCH_AREA_SIZE)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |_, cr, w, h| {
        let cx = (w as f64) / 2.0;
        let cy = (h as f64) / 2.0;
        let stroke_w = 1.3;
        cr.arc(
            cx,
            cy,
            SWATCH_OUTER_RADIUS - stroke_w / 2.0,
            0.0,
            2.0 * std::f64::consts::PI,
        );
        cr.set_line_width(stroke_w);
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.42);
        let _ = cr.stroke();

        // Crisp "+" symbol matching the "×" reset button weight
        let arm = 3.6;
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.move_to(cx - arm, cy);
        cr.line_to(cx + arm, cy);
        cr.move_to(cx, cy - arm);
        cr.line_to(cx, cy + arm);
        cr.set_line_width(1.6);
        cr.set_source_rgba(0.24, 0.22, 0.27, 0.90);
        let _ = cr.stroke();
    });
    btn.set_child(Some(&area));
    btn
}

fn select_swatch_in_row(row: &gtk::Box, active_btn: &gtk::Button) {
    let mut child_opt = row.first_child();
    while let Some(child) = child_opt {
        if &child == active_btn.upcast_ref::<gtk::Widget>() {
            child.add_css_class("selected-swatch");
        } else {
            child.remove_css_class("selected-swatch");
        }
        if let Some(btn) = child.downcast_ref::<gtk::Button>() {
            if let Some(area) = btn.child() {
                area.queue_draw();
            }
        }
        child_opt = child.next_sibling();
    }
}

pub const DROPDOWN_FORMAT_WIDTH: i32 = 80;
pub const DROPDOWN_FONT_FAMILY_WIDTH: i32 = 96;
pub const DROPDOWN_FONT_SIZE_WIDTH: i32 = 44;

fn configure_fixed_width_dropdown(dropdown: &gtk::DropDown, width_request: i32, label_chars: i32) {
    dropdown.set_width_request(width_request);
    dropdown.set_hexpand(false);

    let button_factory = gtk::SignalListItemFactory::new();
    button_factory.connect_setup(move |_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .single_line_mode(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .width_chars(label_chars)
                .max_width_chars(label_chars)
                .build();
            list_item.set_child(Some(&label));
        }
    });
    button_factory.connect_bind(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let text = list_item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            if let Some(label) = list_item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        }
    });

    let list_factory = gtk::SignalListItemFactory::new();
    list_factory.connect_setup(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .single_line_mode(true)
                .margin_start(4)
                .margin_end(4)
                .build();
            list_item.set_child(Some(&label));
        }
    });
    list_factory.connect_bind(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let text = list_item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            if let Some(label) = list_item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        }
    });

    dropdown.set_factory(Some(&button_factory));
    dropdown.set_list_factory(Some(&list_factory));
}

fn configure_style_dropdown_list_preview(dropdown: &gtk::DropDown) {
    let list_factory = gtk::SignalListItemFactory::new();
    list_factory.connect_setup(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .margin_start(6)
                .margin_end(6)
                .margin_top(2)
                .margin_bottom(2)
                .build();
            let title = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .single_line_mode(true)
                .use_markup(true)
                .build();
            let accel = gtk::Label::builder()
                .xalign(1.0)
                .single_line_mode(true)
                .css_classes(["dim-label", "caption"])
                .build();
            row.append(&title);
            row.append(&accel);
            list_item.set_child(Some(&row));
        }
    });
    list_factory.connect_bind(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let pos = list_item.position();
            let text = list_item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            let escaped = glib::markup_escape_text(&text);
            let (markup, shortcut) = match pos {
                1 => (format!("<b><span size=\"large\">{escaped}</span></b>"), "Ctrl+1"),
                2 => (format!("<b><span size=\"medium\">{escaped}</span></b>"), "Ctrl+2"),
                3 => (format!("<b>{escaped}</b>"), "Ctrl+3"),
                4 => (format!("<tt>{escaped}</tt>"), "Ctrl+4"),
                _ => (escaped.to_string(), "Ctrl+0"),
            };
            if let Some(row) = list_item.child().and_downcast::<gtk::Box>() {
                if let Some(title) = row.first_child().and_downcast::<gtk::Label>() {
                    title.set_markup(&markup);
                }
                if let Some(accel) = row.last_child().and_downcast::<gtk::Label>() {
                    accel.set_label(shortcut);
                }
            }
        }
    });
    dropdown.set_list_factory(Some(&list_factory));
}

fn configure_font_dropdown_list_preview(dropdown: &gtk::DropDown) {
    let list_factory = gtk::SignalListItemFactory::new();
    list_factory.connect_setup(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .single_line_mode(true)
                .use_markup(true)
                .margin_start(6)
                .margin_end(6)
                .margin_top(2)
                .margin_bottom(2)
                .build();
            list_item.set_child(Some(&label));
        }
    });
    list_factory.connect_bind(|_, item| {
        if let Some(list_item) = item.downcast_ref::<gtk::ListItem>() {
            let pos = list_item.position() as usize;
            let text = list_item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            let escaped = glib::markup_escape_text(&text);
            let markup = if pos == 0 {
                escaped.to_string()
            } else {
                let face = COMPOSER_FONT_FAMILIES
                    .get(pos)
                    .map(|(name, _)| *name)
                    .unwrap_or("Sans");
                let escaped_face = glib::markup_escape_text(face);
                format!("<span font_family=\"{escaped_face}\">{escaped}</span>")
            };
            if let Some(label) = list_item.child().and_downcast::<gtk::Label>() {
                label.set_markup(&markup);
            }
        }
    });
    dropdown.set_list_factory(Some(&list_factory));
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default, Clone)]
    pub struct CallbackList(pub Vec<std::rc::Rc<dyn Fn(Vec<std::path::PathBuf>) + 'static>>);

    impl std::fmt::Debug for CallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "CallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragMotionCallbackList(pub Vec<std::rc::Rc<dyn Fn(f64, f64) + 'static>>);

    impl std::fmt::Debug for DragMotionCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragMotionCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragLeaveCallbackList(pub Vec<std::rc::Rc<dyn Fn() + 'static>>);

    impl std::fmt::Debug for DragLeaveCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragLeaveCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct ImageConvertCallbackList(
        pub Vec<std::rc::Rc<dyn Fn(crate::services::draft_store_service::DraftAttachment) + 'static>>,
    );

    impl std::fmt::Debug for ImageConvertCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ImageConvertCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct ContentChangedCallbackList(
        pub Vec<std::rc::Rc<dyn Fn(&str, &str) + 'static>>,
    );

    impl std::fmt::Debug for ContentChangedCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ContentChangedCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Default, Clone)]
    pub struct DragInternalCallbackList(
        pub Vec<std::rc::Rc<dyn Fn(bool) + 'static>>,
    );

    impl std::fmt::Debug for DragInternalCallbackList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "DragInternalCallbackList({} callbacks)", self.0.len())
        }
    }

    #[derive(Debug, Default)]
    pub struct HtmlEditor {
        pub web_view: RefCell<Option<webkit6::WebView>>,
        pub web_context: RefCell<Option<webkit6::WebContext>>,
        pub format_toolbar: RefCell<Option<gtk::Box>>,
        pub format_toolbar_revealer: RefCell<Option<gtk::Revealer>>,
        pub btn_typography: RefCell<Option<gtk::MenuButton>>,
        pub typography_popover: RefCell<Option<gtk::Popover>>,
        pub color_popover: RefCell<Option<gtk::Popover>>,
        pub align_popover: RefCell<Option<gtk::Popover>>,
        pub style_buttons: RefCell<Vec<gtk::Button>>,
        pub font_family_buttons: RefCell<Vec<gtk::Button>>,
        pub font_size_label: RefCell<Option<gtk::Label>>,
        pub font_size_buttons: RefCell<Vec<(u32, gtk::Button)>>,
        pub dropdown_format: RefCell<Option<gtk::DropDown>>,
        pub dropdown_font_family: RefCell<Option<gtk::DropDown>>,
        pub dropdown_font_size: RefCell<Option<gtk::DropDown>>,
        pub btn_bold: RefCell<Option<gtk::Button>>,
        pub btn_italic: RefCell<Option<gtk::Button>>,
        pub btn_underline: RefCell<Option<gtk::Button>>,
        pub btn_strikethrough: RefCell<Option<gtk::Button>>,
        pub btn_color: RefCell<Option<gtk::MenuButton>>,
        pub btn_list: RefCell<Option<gtk::Button>>,
        pub btn_ordered_list: RefCell<Option<gtk::Button>>,
        pub btn_blockquote: RefCell<Option<gtk::Button>>,
        pub btn_align: RefCell<Option<gtk::MenuButton>>,
        pub btn_link: RefCell<Option<gtk::MenuButton>>,
        pub link_popover: RefCell<Option<gtk::Popover>>,
        pub btn_table: RefCell<Option<gtk::MenuButton>>,
        pub table_popover: RefCell<Option<gtk::Popover>>,
        pub btn_save_style: RefCell<Option<gtk::Button>>,
        pub btn_reset_style: RefCell<Option<gtk::Button>>,
        pub custom_styles: RefCell<std::collections::BTreeMap<String, CustomBlockStyle>>,
        pub last_text_color: RefCell<String>,
        pub color_indicator_bar: RefCell<Option<gtk::DrawingArea>>,
        pub recent_colors: RefCell<Vec<String>>,
        pub recent_color_row: RefCell<Option<gtk::Box>>,
        pub btn_undo: RefCell<Option<gtk::Button>>,
        pub btn_redo: RefCell<Option<gtk::Button>>,
        pub suppress_toolbar_sync: Cell<bool>,
        pub mode_switcher: RefCell<Option<gtk::Box>>,
        pub mode_html_btn: RefCell<Option<gtk::ToggleButton>>,
        pub mode_txt_btn: RefCell<Option<gtk::ToggleButton>>,
        pub web_view_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub toolbar_scroll: RefCell<Option<gtk::ScrolledWindow>>,
        pub top_bar: RefCell<Option<gtk::Box>>,
        pub canvas_card_mode: Cell<bool>,
        pub plain_text_mode: Cell<bool>,
        pub plain_text_mode_callbacks: RefCell<DragInternalCallbackList>,
        pub web_view_ready: Cell<bool>,
        pub focus_on_ready: Cell<bool>,
        pub pending_scripts: RefCell<Vec<String>>,
        pub spell_checking_enabled: Cell<bool>,
        pub spell_checking_languages: RefCell<Vec<String>>,
        pub auto_format_enabled: Cell<bool>,
        pub spelling_handler_ids: RefCell<Vec<glib::SignalHandlerId>>,
        pub files_dropped_callbacks: RefCell<CallbackList>,
        pub drag_motion_callbacks: RefCell<DragMotionCallbackList>,
        pub drag_leave_callbacks: RefCell<DragLeaveCallbackList>,
        pub drag_internal_callbacks: RefCell<DragInternalCallbackList>,
        pub image_convert_callbacks: RefCell<ImageConvertCallbackList>,
        pub active_image_popover: RefCell<Option<gtk::Popover>>,
        pub content_changed_callbacks: RefCell<ContentChangedCallbackList>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HtmlEditor {
        const NAME: &'static str = "MeridianHtmlEditor";
        type Type = super::HtmlEditor;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for HtmlEditor {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(popover) = self.active_image_popover.borrow_mut().take() {
                popover.unparent();
            }
            if let Some(settings) = AppSettings::try_new() {
                for id in self.spelling_handler_ids.borrow_mut().drain(..) {
                    settings.disconnect(id);
                }
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for HtmlEditor {
        fn realize(&self) {
            self.parent_realize();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorContent {
    pub body: String,
    pub plain_text: bool,
}

glib::wrapper! {
    pub struct HtmlEditor(ObjectSubclass<imp::HtmlEditor>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HtmlEditor {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_files_dropped<F>(&self, callback: F)
    where
        F: Fn(Vec<std::path::PathBuf>) + 'static,
    {
        if let Ok(mut cbs) = self.imp().files_dropped_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn dispatch_files_dropped(&self, paths: Vec<std::path::PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let cbs: Vec<_> = if let Ok(guard) = self.imp().files_dropped_callbacks.try_borrow() {
            guard.0.clone()
        } else {
            Vec::new()
        };
        for cb in cbs {
            let p = paths.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                cb(p);
            }));
        }
    }

    pub fn connect_drag_motion<F>(&self, callback: F)
    where
        F: Fn(f64, f64) + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_motion_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn connect_drag_leave<F>(&self, callback: F)
    where
        F: Fn() + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_leave_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn connect_drag_internal<F>(&self, callback: F)
    where
        F: Fn(bool) + 'static,
    {
        if let Ok(mut cbs) = self.imp().drag_internal_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub fn is_supported_image(path: &std::path::Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp")
    }

    /// Loads an image from path and prepares an optimized data URI.
    /// Raster images exceeding 1920px or 1.5 MB are downscaled using gdk-pixbuf
    /// to avoid passing multi-megabyte Base64 strings through WebKit IPC,
    /// preventing WebKitWebProcess memory bloat and UI freezes.
    pub fn prepare_image_data_uri(path: &std::path::Path) -> Option<(String, String)> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return None;
        };
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let (mime, is_jpeg) = match ext.as_str() {
            "png" => ("image/png", false),
            "jpg" | "jpeg" => ("image/jpeg", true),
            "gif" => ("image/gif", false),
            "svg" => ("image/svg+xml", false),
            "webp" => ("image/webp", false),
            _ => ("image/png", false),
        };

        let is_vector_or_anim = ext == "svg" || ext == "gif";
        let file_size = metadata.len();

        if is_vector_or_anim || file_size < 1_500_000 {
            if !is_vector_or_anim {
                if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file(path) {
                    let w = pixbuf.width();
                    let h = pixbuf.height();
                    if w > 1920 || h > 1920 {
                        let scale = 1920.0 / (w.max(h) as f64);
                        let target_w = (w as f64 * scale).round() as i32;
                        let target_h = (h as f64 * scale).round() as i32;
                        if let Some(scaled) = pixbuf.scale_simple(target_w, target_h, gdk_pixbuf::InterpType::Bilinear) {
                            let format = if is_jpeg { "jpeg" } else { "png" };
                            let options: &[(&str, &str)] = if is_jpeg {
                                &[("quality", "88")]
                            } else {
                                &[]
                            };
                            if let Ok(buf) = scaled.save_to_bufferv(format, options) {
                                let b64 = encode_base64(&buf);
                                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
                            }
                        }
                    }
                }
            }
            if let Ok(bytes) = std::fs::read(path) {
                let b64 = encode_base64(&bytes);
                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
            }
        } else {
            if let Ok(pixbuf) = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 1920, 1920, true) {
                let format = if is_jpeg { "jpeg" } else { "png" };
                let options: &[(&str, &str)] = if is_jpeg {
                    &[("quality", "85")]
                } else {
                    &[]
                };
                if let Ok(buf) = pixbuf.save_to_bufferv(format, options) {
                    let b64 = encode_base64(&buf);
                    return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
                }
            }
            if let Ok(bytes) = std::fs::read(path) {
                let b64 = encode_base64(&bytes);
                return Some((format!("data:{mime};base64,{b64}"), mime.to_string()));
            }
        }

        None
    }

    pub fn insert_image_from_path(&self, path: &std::path::Path) -> bool {
        if self.plain_text_mode() || !Self::is_supported_image(path) {
            return false;
        }
        let Some((data_uri, _)) = Self::prepare_image_data_uri(path) else {
            return false;
        };
        let js = format!(
            r#"(function() {{
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    const uri = '{data_uri}';
    let sel = window.getSelection();
    const editor = document.body;
    const sig = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');

    function isBlockEmpty(el) {{
        if (!el) return true;
        if (el.nodeType === 3) return el.textContent.replace(/[\s\u200b\u00a0]+/g, '') === '';
        const text = el.textContent ? el.textContent.replace(/[\s\u200b\u00a0]+/g, '') : '';
        if (text === '') {{
            const imgs = el.querySelectorAll ? el.querySelectorAll('img') : [];
            return imgs.length === 0;
        }}
        return false;
    }}

    function isInsideOrAfterSig(node, offset) {{
        if (!sig || !node) return false;
        if (sig.contains(node)) return true;
        try {{
            const cmp = sig.compareDocumentPosition(node);
            if (cmp & Node.DOCUMENT_POSITION_FOLLOWING) return true;
            if (node === editor) {{
                const sigIdx = Array.prototype.indexOf.call(editor.childNodes, sig);
                if (sigIdx !== -1 && offset > sigIdx) return true;
            }}
        }} catch(e) {{}}
        return false;
    }}

    let validSel = sel && sel.rangeCount > 0 && editor.contains(sel.anchorNode);
    if (validSel && sig && isInsideOrAfterSig(sel.anchorNode, sel.anchorOffset)) {{
        validSel = false;
    }}

    const img = document.createElement('img');
    img.src = uri;
    img.style.maxWidth = '100%';
    img.style.height = 'auto';
    img.setAttribute('draggable', 'true');
    img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);
    img.setAttribute('data-meridian-id', img.id);

    let inserted = false;

    // Check if drop was marked as top
    const wasTop = !!window.__lastDropWasTop;
    if (window.__lastDropWasTop) {{
        window.__lastDropWasTop = false;
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{
            firstBlock.innerHTML = '';
            firstBlock.appendChild(img);
            inserted = true;
        }} else {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            if (firstBlock) {{
                editor.insertBefore(p, firstBlock);
            }} else if (sig) {{
                editor.insertBefore(p, sig);
            }} else {{
                editor.appendChild(p);
            }}
            inserted = true;
        }}
    }}

    if (!inserted && validSel) {{
        const range = sel.getRangeAt(0);
        let container = range.startContainer;
        let block = (container.nodeType === 1 ? container : container.parentElement)?.closest('p, div, li, blockquote');

        if (container === editor) {{
            const childAtOffset = editor.childNodes[range.startOffset];
            const prevChild = range.startOffset > 0 ? editor.childNodes[range.startOffset - 1] : null;
            if (prevChild && isBlockEmpty(prevChild) && prevChild !== sig) {{
                block = prevChild;
            }} else if (childAtOffset && isBlockEmpty(childAtOffset) && childAtOffset !== sig) {{
                block = childAtOffset;
            }}
        }}

        if (block && isBlockEmpty(block) && block !== sig) {{
            block.innerHTML = '';
            block.appendChild(img);
            inserted = true;
        }} else if (container.nodeType === 1 && container.childNodes.length === 1 && container.childNodes[0].nodeName === 'BR') {{
            container.innerHTML = '';
            container.appendChild(img);
            inserted = true;
        }} else {{
            range.deleteContents();
            if (container.nodeType === 1 && container.children.length === 1 && container.children[0].nodeName === 'BR') {{
                container.innerHTML = '';
                container.appendChild(img);
            }} else if (container === editor) {{
                const p = document.createElement('p');
                p.style.margin = '4px 0';
                p.appendChild(img);
                range.insertNode(p);
            }} else {{
                range.insertNode(img);
            }}
            inserted = true;
        }}
    }}

    if (!inserted) {{
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{
            firstBlock.innerHTML = '';
            firstBlock.appendChild(img);
        }} else if (sig) {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            editor.insertBefore(p, sig);
        }} else {{
            const p = document.createElement('p');
            p.style.margin = '4px 0';
            p.appendChild(img);
            editor.appendChild(p);
        }}
    }}

    // Clean up empty first block only if dropped at top
    if (wasTop) {{
        let firstBlock = null;
        for (let i = 0; i < editor.children.length; i++) {{
            const child = editor.children[i];
            if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;
            firstBlock = child;
            break;
        }}
        if (firstBlock && firstBlock !== sig && isBlockEmpty(firstBlock) && !firstBlock.contains(img)) {{
            firstBlock.remove();
        }}
    }}

    // Set selection after image
    try {{
        const afterRange = document.createRange();
        afterRange.setStartAfter(img);
        afterRange.collapse(true);
        sel = window.getSelection();
        if (sel) {{
            sel.removeAllRanges();
            sel.addRange(afterRange);
        }}
    }} catch(e) {{}}
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();
}})();"#
        );
        if let Some(wv) = self.imp().web_view.borrow().as_ref() {
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
            true
        } else {
            false
        }
    }

    pub fn connect_image_convert_to_attachment<F>(&self, callback: F)
    where
        F: Fn(crate::services::draft_store_service::DraftAttachment) + 'static,
    {
        if let Ok(mut cbs) = self.imp().image_convert_callbacks.try_borrow_mut() {
            cbs.0.push(std::rc::Rc::new(callback));
        }
    }

    pub(crate) fn handle_image_menu_script_message(&self, json: &str) {
        let unquoted: String = serde_json::from_str(json).unwrap_or_else(|_| json.to_string());
        let Ok(payload) = serde_json::from_str::<ImageMenuPayload>(&unquoted)
            .or_else(|_| serde_json::from_str::<ImageMenuPayload>(json))
        else {
            return;
        };
        self.present_image_context_popover(&payload);
    }

    fn present_image_context_popover(&self, payload: &ImageMenuPayload) {
        if let Some(old_pop) = self.imp().active_image_popover.borrow_mut().take() {
            old_pop.unparent();
        }

        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };

        let popover = gtk::Popover::builder()
            .has_arrow(true)
            .pointing_to(&gdk::Rectangle::new(payload.x, payload.y, 1, 1))
            .css_classes(["image-options-popover"])
            .build();
        popover.set_parent(&web_view);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.set_margin_top(8);
        content.set_margin_bottom(8);

        // Header
        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let header_icon = gtk::Image::from_icon_name("insert-image-symbolic");
        header_icon.set_pixel_size(16);
        let header_label = gtk::Label::builder()
            .label(gettext("Inline Image"))
            .css_classes(["heading"])
            .xalign(0.0)
            .build();
        header_box.append(&header_icon);
        header_box.append(&header_label);
        content.append(&header_box);

        // 1. Alignment section
        let align_label = gtk::Label::builder()
            .label(gettext("Alignment"))
            .css_classes(["dim-label", "caption"])
            .xalign(0.0)
            .build();
        content.append(&align_label);

        let align_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        align_box.add_css_class("linked");

        let btn_align_left = gtk::Button::builder()
            .icon_name("format-justify-left-symbolic")
            .tooltip_text(gettext("Align Left"))
            .build();
        let btn_align_center = gtk::Button::builder()
            .icon_name("format-justify-center-symbolic")
            .tooltip_text(gettext("Center"))
            .build();
        let btn_align_right = gtk::Button::builder()
            .icon_name("format-justify-right-symbolic")
            .tooltip_text(gettext("Align Right"))
            .build();
        let btn_align_wrap = gtk::Button::builder()
            .icon_name("format-text-direction-ltr-symbolic")
            .tooltip_text(gettext("Wrap Left"))
            .build();

        match payload.align.as_str() {
            "left" => btn_align_left.add_css_class("suggested-action"),
            "center" => btn_align_center.add_css_class("suggested-action"),
            "right" => btn_align_right.add_css_class("suggested-action"),
            _ => btn_align_wrap.add_css_class("suggested-action"),
        }

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_left.connect_clicked(move |_| {
            b_l.add_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = '0'; el.style.marginRight = 'auto'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_center.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.add_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = 'auto'; el.style.marginRight = 'auto'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_right.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'block'; el.style.marginLeft = 'auto'; el.style.marginRight = '0'; el.style.float = 'none'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (b_l, b_c, b_r, b_w) = (
            btn_align_left.clone(),
            btn_align_center.clone(),
            btn_align_right.clone(),
            btn_align_wrap.clone(),
        );
        btn_align_wrap.connect_clicked(move |_| {
            b_l.remove_css_class("suggested-action");
            b_c.remove_css_class("suggested-action");
            b_r.remove_css_class("suggested-action");
            b_w.add_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.display = 'inline'; el.style.float = 'left'; el.style.margin = '4px 14px 6px 0'; el.removeAttribute('align'); }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        align_box.append(&btn_align_left);
        align_box.append(&btn_align_center);
        align_box.append(&btn_align_right);
        align_box.append(&btn_align_wrap);
        content.append(&align_box);

        // 2. Size section
        let size_label = gtk::Label::builder()
            .label(gettext("Size"))
            .css_classes(["dim-label", "caption"])
            .xalign(0.0)
            .build();
        content.append(&size_label);

        let size_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        size_box.add_css_class("linked");

        let btn_size_25 = gtk::Button::builder()
            .label("25%")
            .tooltip_text(gettext("Small (25%)"))
            .build();
        let btn_size_50 = gtk::Button::builder()
            .label("50%")
            .tooltip_text(gettext("Medium (50%)"))
            .build();
        let btn_size_100 = gtk::Button::builder()
            .label("100%")
            .tooltip_text(gettext("Original (100%)"))
            .build();
        let btn_size_orig = gtk::Button::builder()
            .label(gettext("Original"))
            .tooltip_text(gettext("Original Size"))
            .build();

        match payload.size.as_str() {
            "small" => btn_size_25.add_css_class("suggested-action"),
            "medium" => btn_size_50.add_css_class("suggested-action"),
            "large" => btn_size_100.add_css_class("suggested-action"),
            _ => btn_size_orig.add_css_class("suggested-action"),
        }

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_25.connect_clicked(move |_| {
            s25.add_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '25%'; el.style.maxWidth = '25%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_50.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.add_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '50%'; el.style.maxWidth = '50%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_100.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.add_css_class("suggested-action");
            sorig.remove_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = '100%'; el.style.maxWidth = '100%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        let id = payload.id.clone();
        let wv = web_view.clone();
        let (s25, s50, s100, sorig) = (
            btn_size_25.clone(),
            btn_size_50.clone(),
            btn_size_100.clone(),
            btn_size_orig.clone(),
        );
        btn_size_orig.connect_clicked(move |_| {
            s25.remove_css_class("suggested-action");
            s50.remove_css_class("suggested-action");
            s100.remove_css_class("suggested-action");
            sorig.add_css_class("suggested-action");
            let js = format!("(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ el.style.width = 'auto'; el.style.maxWidth = '100%'; el.style.height = 'auto'; }} }})()");
            wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
        });

        size_box.append(&btn_size_25);
        size_box.append(&btn_size_50);
        size_box.append(&btn_size_100);
        size_box.append(&btn_size_orig);
        content.append(&size_box);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let make_action_btn = |icon: &str, label: &str, tooltip: &str, is_destr: bool| {
            let btn = gtk::Button::builder()
                .css_classes(if is_destr {
                    vec!["flat", "destructive-action"]
                } else {
                    vec!["flat"]
                })
                .halign(gtk::Align::Fill)
                .tooltip_text(tooltip)
                .build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let img = gtk::Image::from_icon_name(icon);
            img.set_pixel_size(16);
            let lbl = gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .build();
            row.append(&img);
            row.append(&lbl);
            btn.set_child(Some(&row));
            btn
        };

        // Convert to attachment
        let btn_convert = make_action_btn(
            "mail-attachment-symbolic",
            &gettext("Convert to Attachment"),
            &gettext("Removes the image from the message body and attaches it as a file"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_convert.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.convert_image_to_attachment(&img_id, false);
            }
        });
        content.append(&btn_convert);

        // Also attach
        let btn_attach_also = make_action_btn(
            "list-add-symbolic",
            &gettext("Also Attach as File"),
            &gettext("Keeps the image in the message body and additionally attaches it as a file"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_attach_also.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.convert_image_to_attachment(&img_id, true);
            }
        });
        content.append(&btn_attach_also);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Copy image
        let btn_copy = make_action_btn(
            "edit-copy-symbolic",
            &gettext("Copy Image"),
            &gettext("Copies the image to the clipboard"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_copy.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.copy_image_to_clipboard(&img_id);
            }
        });
        content.append(&btn_copy);

        // Save image as
        let btn_save = make_action_btn(
            "document-save-as-symbolic",
            &gettext("Save Image As…"),
            &gettext("Saves the image to disk"),
            false,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_save.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.save_image_as(&img_id);
            }
        });
        content.append(&btn_save);

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        // Remove image
        let btn_remove = make_action_btn(
            "user-trash-symbolic",
            &gettext("Remove Image"),
            &gettext("Deletes the image from the email"),
            true,
        );
        let editor_weak = self.downgrade();
        let img_id = payload.id.clone();
        let pop_weak = popover.downgrade();
        btn_remove.connect_clicked(move |_| {
            if let Some(pop) = pop_weak.upgrade() {
                pop.popdown();
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.remove_image(&img_id);
            }
        });
        content.append(&btn_remove);

        let editor_weak = self.downgrade();
        popover.connect_closed(move |p| {
            let p_clone = p.clone();
            let ed_weak = editor_weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(editor) = ed_weak.upgrade() {
                    let mut cur = editor.imp().active_image_popover.borrow_mut();
                    if let Some(active) = cur.as_ref() {
                        if active == &p_clone {
                            p_clone.unparent();
                            *cur = None;
                        }
                    }
                }
            });
        });

        popover.set_child(Some(&content));
        self.imp().active_image_popover.replace(Some(popover.clone()));
        popover.popup();
    }

    pub fn convert_image_to_attachment(&self, img_id: &str, keep_inline: bool) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let js = if keep_inline {
            format!(
                "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; return JSON.stringify({{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}); }})()",
                id = img_id
            )
        } else {
            format!(
                "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; const data = {{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}; const p = el.parentNode; el.remove(); if (p && p !== document.body && p.childNodes.length === 0) {{ p.remove(); }} return JSON.stringify(data); }})()",
                id = img_id
            )
        };

        let editor_weak = self.downgrade();
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    if s != "null" && s != "undefined" && !s.is_empty() {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.process_converted_image_json(&s);
                        }
                    }
                }
            },
        );
    }

    fn process_converted_image_json(&self, json_str: &str) {
        let parsed_str: String = serde_json::from_str(json_str).unwrap_or_else(|_| json_str.to_string());
        let Ok(data) = serde_json::from_str::<ConvertedImageData>(&parsed_str)
            .or_else(|_| serde_json::from_str::<ConvertedImageData>(json_str))
        else {
            return;
        };
        let Some((bytes, mime, filename)) = parse_image_data_uri_or_path(&data.src, &data.alt) else {
            return;
        };
        let temp_filename = format!("meridian_att_{}_{}", &glib::uuid_string_random()[..8], filename);
        let temp_path = std::env::temp_dir().join(&temp_filename);
        let _ = std::fs::write(&temp_path, &bytes);
        let b64 = encode_base64(&bytes);

        let attachment = crate::services::draft_store_service::DraftAttachment {
            path: temp_path.to_string_lossy().to_string(),
            filename,
            mime_type: mime,
            size: bytes.len() as u64,
            content_id: None,
            embedded_content_base64: b64,
        };

        let cbs: Vec<_> = if let Ok(guard) = self.imp().image_convert_callbacks.try_borrow() {
            guard.0.clone()
        } else {
            Vec::new()
        };
        for cb in cbs {
            let att = attachment.clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                cb(att);
            }));
        }
    }

    pub fn copy_image_to_clipboard(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let editor_weak = self.downgrade();
        let js = format!(
            "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); return el ? el.src : null; }})()",
            id = img_id
        );
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    if let Some((bytes, _, _)) = parse_image_data_uri_or_path(&s, "") {
                        let glib_bytes = glib::Bytes::from_owned(bytes);
                        if let Ok(texture) = gdk::Texture::from_bytes(&glib_bytes) {
                            if let Some(editor) = editor_weak.upgrade() {
                                editor.display().clipboard().set_texture(&texture);
                            }
                        }
                    }
                }
            },
        );
    }

    pub fn save_image_as(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let parent = self.root().and_downcast::<gtk::Window>();
        let js = format!(
            "(function(){{ const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (!el) return null; return JSON.stringify({{ src: el.src, alt: el.getAttribute('alt') || el.getAttribute('title') || '' }}); }})()",
            id = img_id
        );
        wv.evaluate_javascript(
            &js,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(js_val) = result {
                    let s = js_val.to_str();
                    let parsed_str: String = serde_json::from_str::<String>(&s).unwrap_or_else(|_| s.to_string());
                    let Ok(data) = serde_json::from_str::<ConvertedImageData>(&parsed_str)
                        .or_else(|_| serde_json::from_str::<ConvertedImageData>(&s))
                    else {
                        return;
                    };
                    if let Some((bytes, _, filename)) = parse_image_data_uri_or_path(&data.src, &data.alt) {
                        let file_dialog = gtk::FileDialog::builder()
                            .title(gettext("Save Image As…"))
                            .initial_name(&filename)
                            .modal(true)
                            .build();
                        file_dialog.save(parent.as_ref(), None::<&gio::Cancellable>, move |res| {
                            if let Ok(file) = res {
                                if let Some(path) = file.path() {
                                    let _ = std::fs::write(&path, &bytes);
                                }
                            }
                        });
                    }
                }
            },
        );
    }

    pub fn remove_image(&self, img_id: &str) {
        let Some(wv) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        let js = format!(
            "(function(){{ if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); const el = document.getElementById('{id}') || document.querySelector('img[data-meridian-id=\"{id}\"]'); if (el) {{ const p = el.parentNode; el.remove(); if (p && p !== document.body && p.childNodes.length === 0) {{ p.remove(); }} if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); }} }})()",
            id = img_id
        );
        wv.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
    }

    pub fn cancel_drag_session(&self) {
        if let Some(wv) = self.imp().web_view.borrow().as_ref() {
            wv.evaluate_javascript(
                "if (typeof resetDraggedElement === 'function') resetDraggedElement(); if (isInternalDrag) { try { window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); } catch(err) {} } hideCaret(); dragCounter = 0; currentDropRange = null; currentDropInfo = null; isInternalDrag = false; draggedElement = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    }

    pub(crate) fn handle_drag_script_message(&self, msg: &str) {
        if msg == "enter" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_motion_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(0.0, 100.0);
                }));
            }
        } else if let Some(coords) = msg.strip_prefix("motion:") {
            let mut parts = coords.split(':');
            if let (Some(y_str), Some(h_str)) = (parts.next(), parts.next()) {
                if let (Ok(y), Ok(h)) = (y_str.parse::<f64>(), h_str.parse::<f64>()) {
                    let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_motion_callbacks.try_borrow() {
                        guard.0.clone()
                    } else {
                        Vec::new()
                    };
                    for cb in cbs {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            cb(y, h);
                        }));
                    }
                }
            }
        } else if msg == "internal_start" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_internal_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(true);
                }));
            }
        } else if msg == "internal_end" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_internal_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb(false);
                }));
            }
        } else if msg == "leave" || msg == "drop" || msg == "cancel" {
            let cbs: Vec<_> = if let Ok(guard) = self.imp().drag_leave_callbacks.try_borrow() {
                guard.0.clone()
            } else {
                Vec::new()
            };
            for cb in cbs {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    cb();
                }));
            }
        }
    }

    /// Read the current editor contents asynchronously on the GTK main
    /// context.  WebKit owns the DOM, so callers must not try to access it
    /// synchronously or from a worker thread.
    pub fn read_html<F>(&self, callback: F)
    where
        F: FnOnce(Result<String, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };

        web_view.evaluate_javascript(
            "document.body.innerHTML",
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| callback(result.map(|value| value.to_str().to_string())),
        );
    }

    /// Read the representation that should be persisted or sent.
    /// Automatically sends plain text (`plain_text: true`) when no rich HTML
    /// formatting, links, lists, quotes, or images were used in the message;
    /// otherwise retains the full HTML representation (`plain_text: false`).
    pub fn read_content<F>(&self, callback: F)
    where
        F: FnOnce(Result<EditorContent, glib::Error>) + 'static,
    {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            callback(Err(glib::Error::new(
                glib::FileError::Failed,
                "HTML editor is not initialized",
            )));
            return;
        };
        let force_plain_text = self.plain_text_mode();
        let expression = if force_plain_text {
            r#"JSON.stringify({ body: (typeof window.meridianExtractPlainText === 'function' ? window.meridianExtractPlainText(document.body) : document.body.innerText), plain_text: true })"#
        } else {
            r#"(() => {
                const customMap = window.__meridianCustomStyles || {};
                const hasCustomStyles = Object.keys(customMap).length > 0;
                const richSelector = 'b, strong, i, em, u, s, strike, del, font, a[href], img, ul, ol, li, blockquote, h1, h2, h3, h4, h5, h6, hr, pre, code, table, sup, sub, [style*="color"], [style*="font-"], [style*="text-align"], [style*="background"]';
                const hasFormatting = hasCustomStyles
                    || document.body.querySelector(richSelector) !== null
                    || Boolean(document.body.style && document.body.style.fontFamily);
                if (!hasFormatting) {
                    const plain = typeof window.meridianExtractPlainText === 'function'
                        ? window.meridianExtractPlainText(document.body)
                        : document.body.innerText;
                    return JSON.stringify({ body: plain, plain_text: true });
                }
                const clone = document.body.cloneNode(true);
                for (const tag of ['p', 'h1', 'h2', 'h3', 'pre']) {
                    const customCss = customMap[tag] || '';
                    if (customCss) {
                        clone.querySelectorAll(tag).forEach(el => {
                            const existing = el.getAttribute('style') || '';
                            el.setAttribute('style', existing ? (customCss + ' ' + existing) : customCss);
                        });
                    }
                }
                clone.querySelectorAll('p').forEach(p => {
                    const st = p.getAttribute('style') || '';
                    if (!/margin/i.test(st)) {
                        p.setAttribute('style', st ? ('margin: 0; ' + st) : 'margin: 0;');
                    }
                });
                return JSON.stringify({
                    body: clone.innerHTML,
                    plain_text: false
                });
            })()"#
        };
        web_view.evaluate_javascript(
            expression,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                callback(result.map(|value| {
                    let raw = value.to_str();
                    if let Ok(parsed) = serde_json::from_str::< serde_json::Value >(&raw) {
                        let body = parsed
                            .get("body")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let plain_text = parsed
                            .get("plain_text")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(force_plain_text);
                        EditorContent { body, plain_text }
                    } else {
                        EditorContent {
                            body: raw.to_string(),
                            plain_text: force_plain_text,
                        }
                    }
                }))
            },
        );
    }

    /// Replace the editable DOM contents.  The HTML is encoded as a JSON
    /// string before it is passed to JavaScript so quoted text from a
    /// reply/forward or a restored draft cannot become executable code.
    pub fn set_html(&self, html: &str) {
        let Ok(encoded) = serde_json::to_string(html) else {
            return;
        };
        let caret_js = "try { const first = document.body.firstElementChild || document.body.firstChild; if (first) { const range = document.createRange(); range.setStart(first, 0); range.collapse(true); const sel = window.getSelection(); if (sel) { sel.removeAllRanges(); sel.addRange(range); } } } catch (e) {}";
        let script = if self.plain_text_mode() {
            format!(
                "document.body.innerHTML = {encoded}; document.body.textContent = document.body.innerText; {caret_js} if (typeof window.meridianResetHistory === 'function') window.meridianResetHistory(); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();"
            )
        } else {
            format!("document.body.innerHTML = {encoded}; {caret_js} if (typeof window.meridianResetHistory === 'function') window.meridianResetHistory(); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();")
        };
        self.evaluate_or_queue(script);
    }

    /// Replace the editor with literal text, regardless of the current mode.
    pub fn set_text(&self, text: &str) {
        let Ok(encoded) = serde_json::to_string(text) else {
            return;
        };
        self.evaluate_or_queue(format!(
            "document.body.textContent = {encoded}; if (typeof window.meridianResetHistory === 'function') window.meridianResetHistory(); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();"
        ));
    }

    /// Insert sanitized HTML at the current selection without replacing the
    /// rest of the message.  In plain-text mode the same fragment is reduced
    /// to literal text inside the DOM before it is inserted.
    pub fn insert_html(&self, html: &str) {
        let safe_html = crate::services::html_sanitizer::sanitize_html(html);
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = if self.plain_text_mode() {
            format!(
                "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); const snippet = document.createElement('div'); snippet.innerHTML = {encoded}; document.execCommand('insertText', false, snippet.innerText); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);"
            )
        } else {
            format!(
                "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('insertHTML', false, {encoded}); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);"
            )
        };
        self.evaluate_or_queue(script);
    }

    /// Replace an existing signature in the DOM or insert a new one.
    /// If `signature_html` is empty, any existing signature element is removed.
    pub fn replace_or_insert_signature(&self, signature_html: &str, top_signature: bool) {
        let trimmed = signature_html.trim();
        let is_empty = trimmed.is_empty();
        let safe_html = if is_empty {
            String::new()
        } else {
            crate::services::html_sanitizer::sanitize_html(trimmed)
        };
        let Ok(encoded) = serde_json::to_string(&safe_html) else {
            return;
        };
        let script = format!(
            r#"(function() {{
    const el = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');
    const isEmpty = {is_empty};
    if (isEmpty) {{
        if (el) {{
            el.remove();
        }}
        if (window.__meridianUserEdited) {{
            if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
        }} else if (typeof window.meridianResetHistory === 'function') {{
            window.meridianResetHistory();
        }}
        return;
    }}
    const temp = document.createElement('div');
    temp.innerHTML = {encoded};
    const newSig = temp.firstElementChild || temp;
    const findQuoteTarget = () => {{
        const attr = document.querySelector('.meridian-quote-attribution');
        if (attr) return attr;
        const quote = document.querySelector('blockquote');
        if (quote) {{
            let prev = quote.previousElementSibling;
            while (prev && (prev === el || (prev.tagName === 'P' && prev.textContent.trim() === ''))) {{
                prev = prev.previousElementSibling;
            }}
            if (prev && (prev.tagName === 'P' || prev.tagName === 'DIV') && prev.textContent.trim().length > 0) {{
                return prev;
            }}
            return quote;
        }}
        return null;
    }};
    if (el) {{
        const target = {top_signature} ? findQuoteTarget() : null;
        if (target && (target.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING)) {{
            el.remove();
            target.parentNode.insertBefore(newSig, target);
            const next = newSig.nextElementSibling;
            if (!next || next === target) {{
                const gap = document.createElement('p');
                gap.innerHTML = '<br>';
                target.parentNode.insertBefore(gap, target);
            }}
        }} else {{
            el.replaceWith(newSig);
        }}
    }} else {{
        if ({top_signature}) {{
            const target = findQuoteTarget();
            if (target) {{
                target.parentNode.insertBefore(newSig, target);
                const gap = document.createElement('p');
                gap.innerHTML = '<br>';
                target.parentNode.insertBefore(gap, target);
            }} else {{
                document.body.prepend(newSig);
            }}
        }} else {{
            const br = document.createElement('p');
            br.innerHTML = '<br>';
            document.body.appendChild(br);
            document.body.appendChild(newSig);
        }}
    }}
    if ({top_signature}) {{
        let emptyBefore = 0;
        let curr = newSig.previousElementSibling;
        while (curr) {{
            emptyBefore++;
            curr = curr.previousElementSibling;
        }}
        while (emptyBefore < 2) {{
            const p = document.createElement('p');
            p.innerHTML = '<br>';
            newSig.parentNode.insertBefore(p, document.body.firstChild);
            emptyBefore++;
        }}
        try {{
            const first = document.body.firstElementChild || document.body.firstChild;
            if (first) {{
                const range = document.createRange();
                range.setStart(first, 0);
                range.collapse(true);
                const sel = window.getSelection();
                if (sel) {{
                    sel.removeAllRanges();
                    sel.addRange(range);
                }}
            }}
            document.body.focus();
        }} catch (e) {{}}
    }}
    if (window.__meridianUserEdited) {{
        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    }} else if (typeof window.meridianResetHistory === 'function') {{
        window.meridianResetHistory();
    }}
}})();"#
        );
        self.evaluate_or_queue(script);
    }

    pub fn undo(&self) {
        self.evaluate_or_queue(
            "if (typeof window.meridianUndo === 'function') { window.meridianUndo(); } else { document.execCommand('undo', false, null); }".to_string(),
        );
    }

    pub fn redo(&self) {
        self.evaluate_or_queue(
            "if (typeof window.meridianRedo === 'function') { window.meridianRedo(); } else { document.execCommand('redo', false, null); }".to_string(),
        );
    }

    pub fn select_all(&self) {
        self.evaluate_or_queue(
            "document.execCommand('selectAll', false, null); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn toggle_bold(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('bold', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn toggle_italic(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('italic', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn toggle_underline(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('underline', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn toggle_strikethrough(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('strikethrough', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn clear_formatting(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('removeFormat', false, null); if (typeof window.meridianUnwrapBlockquote === 'function') { while (window.meridianUnwrapBlockquote()) {} } document.execCommand('formatBlock', false, 'p'); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub fn copy_formatting(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianCopyStyle === 'function') { window.meridianCopyStyle(); }".to_string(),
        );
    }

    pub fn apply_copied_formatting(&self) {
        if self.plain_text_mode() {
            return;
        }
        self.evaluate_or_queue(
            "if (typeof window.meridianApplyCopiedStyle === 'function') { window.meridianApplyCopiedStyle(); }".to_string(),
        );
    }

    pub fn insert_plain_text_matching_style(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Ok(encoded) = serde_json::to_string(text) else {
            return;
        };
        self.evaluate_or_queue(format!(
            "if (typeof window.meridianInsertPlainTextMatchStyle === 'function') {{ window.meridianInsertPlainTextMatchStyle({encoded}); }} else {{ if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('insertText', false, {encoded}); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); }}"
        ));
    }

    /// Smart `Ctrl+Shift+V` handler:
    /// - If the user copied formatting (`Ctrl+Shift+C`) and has a non-collapsed text selection,
    ///   applies the copied formatting directly to the selected text without replacing it.
    /// - Otherwise, reads plain text from the clipboard and inserts it matching the current
    ///   cursor formatting (`insertText`), stripping all foreign HTML/backgrounds/fonts.
    pub fn paste_or_apply_formatting(&self) {
        if self.plain_text_mode() {
            self.paste_plain_text_from_clipboard();
            return;
        }
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.imp().web_view_ready.get() {
            self.apply_copied_formatting();
            return;
        }
        let editor_weak = self.downgrade();
        web_view.evaluate_javascript(
            r#"(() => {
                const sel = window.getSelection();
                if (window.__meridianCopiedStyle && sel && !sel.isCollapsed && sel.toString().length > 0 && typeof window.meridianApplyCopiedStyle === 'function') {
                    return window.meridianApplyCopiedStyle() ? 'applied' : 'paste';
                }
                return 'paste';
            })()"#,
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                let applied = result
                    .map(|val| val.to_str() == "applied")
                    .unwrap_or(false);
                if !applied {
                    if let Some(editor) = editor_weak.upgrade() {
                        editor.paste_plain_text_from_clipboard();
                    }
                }
            },
        );
    }

    fn paste_plain_text_from_clipboard(&self) {
        let clipboard = self.display().clipboard();
        let editor_weak = self.downgrade();
        clipboard.read_text_async(None::<&gio::Cancellable>, move |res| {
            if let Ok(Some(text)) = res {
                if !text.is_empty() {
                    if let Some(editor) = editor_weak.upgrade() {
                        editor.insert_plain_text_matching_style(&text);
                    }
                }
            }
        });
    }

    pub fn open_link_popover(&self) {
        if self.plain_text_mode() {
            return;
        }
        if !self.is_format_toolbar_visible() {
            self.set_format_toolbar_visible(true);
        }
        if let Some(popover) = self.imp().link_popover.borrow().as_ref() {
            popover.popup();
        } else if let Some(btn) = self.imp().btn_link.borrow().as_ref() {
            btn.popup();
        }
    }

    pub fn set_text_color(&self, color: &str) {
        if self.plain_text_mode() {
            return;
        }
        self.imp().last_text_color.replace(color.to_string());
        if let Some(bar) = self.imp().color_indicator_bar.borrow().as_ref() {
            bar.queue_draw();
        }
        if !color.is_empty() {
            self.record_recent_color(color);
        }
        let script = if color.is_empty() {
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('styleWithCSS', false, true); document.execCommand('foreColor', false, 'inherit'); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string()
        } else if let Ok(encoded) = serde_json::to_string(color) {
            format!(
                "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('styleWithCSS', false, true); document.execCommand('foreColor', false, {encoded}); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();"
            )
        } else {
            return;
        };
        self.evaluate_or_queue(script);
    }

    pub fn record_recent_color(&self, color: &str) {
        let trimmed = color.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("transparent")
            || trimmed.eq_ignore_ascii_case("inherit")
        {
            return;
        }
        {
            let mut recent = self.imp().recent_colors.borrow_mut();
            recent.retain(|c| !c.eq_ignore_ascii_case(trimmed));
            recent.insert(0, trimmed.to_string());
            if recent.len() > MAX_RECENT_COMPOSER_COLORS {
                recent.truncate(MAX_RECENT_COMPOSER_COLORS);
            }
            save_recent_composer_colors(&recent);
        }
        self.rebuild_recent_colors_row();
    }

    pub(crate) fn rebuild_recent_colors_row(&self) {
        let Some(row) = self.imp().recent_color_row.borrow().clone() else {
            return;
        };
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        let colors = self.imp().recent_colors.borrow().clone();
        if colors.is_empty() {
            row.set_visible(false);
            return;
        }
        row.set_visible(true);
        let pop_weak = self
            .imp()
            .color_popover
            .borrow()
            .as_ref()
            .map(|p| p.downgrade());

        for hex in colors.iter().take(MAX_RECENT_COMPOSER_COLORS) {
            let swatch = build_cairo_color_swatch_button(hex, hex);
            let editor_weak = self.downgrade();
            let pop_w = pop_weak.clone();
            let hex_str = hex.clone();
            swatch.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_color(&hex_str);
                }
                if let Some(pw) = pop_w.as_ref().and_then(|w| w.upgrade()) {
                    pw.popdown();
                }
            });
            row.append(&swatch);
        }
    }

    pub fn apply_custom_styles_to_dom(&self) {
        let styles = self.imp().custom_styles.borrow().clone();
        let css = build_custom_styles_css(&styles);
        let inline_map: std::collections::BTreeMap<String, String> = styles
            .iter()
            .filter_map(|(tag, st)| {
                let inline = st.to_inline_css();
                if inline.is_empty() {
                    None
                } else {
                    Some((tag.clone(), inline))
                }
            })
            .collect();
        let Ok(css_json) = serde_json::to_string(&css) else {
            return;
        };
        let Ok(map_json) = serde_json::to_string(&inline_map) else {
            return;
        };
        self.evaluate_or_queue(format!(
            "if (typeof window.meridianSetCustomStyles === 'function') {{ window.meridianSetCustomStyles({map_json}, {css_json}); }}"
        ));
    }

    pub fn save_current_style_preset(&self) {
        let tag = match self
            .imp()
            .dropdown_format
            .borrow()
            .as_ref()
            .map(|dd| dd.selected())
            .unwrap_or(0)
        {
            1 => "h1",
            2 => "h2",
            3 => "h3",
            4 => "pre",
            _ => "p",
        };
        let ff_idx = self
            .imp()
            .dropdown_font_family
            .borrow()
            .as_ref()
            .map(|dd| dd.selected() as usize)
            .unwrap_or(0);
        let font_family = COMPOSER_FONT_FAMILIES
            .get(ff_idx)
            .map(|(_, css)| (*css).to_string())
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let fs_idx = self
            .imp()
            .dropdown_font_size
            .borrow()
            .as_ref()
            .map(|dd| dd.selected())
            .unwrap_or(0);
        let font_size_px = {
            let px = font_size_px_for_index(fs_idx);
            if px == 0 {
                AppSettings::try_new()
                    .map(|s| s.mail_font_size())
                    .unwrap_or(14)
            } else {
                px
            }
        };
        let bold = self
            .imp()
            .btn_bold
            .borrow()
            .as_ref()
            .map(|b| b.has_css_class("suggested-action"));
        let italic = self
            .imp()
            .btn_italic
            .borrow()
            .as_ref()
            .map(|b| b.has_css_class("suggested-action"));
        let underline = self
            .imp()
            .btn_underline
            .borrow()
            .as_ref()
            .map(|b| b.has_css_class("suggested-action"));
        let color = self.imp().last_text_color.borrow().clone();

        let preset = CustomBlockStyle {
            font_family: font_family.clone(),
            font_size_px,
            bold,
            italic,
            underline,
            color,
        };
        {
            let mut styles = self.imp().custom_styles.borrow_mut();
            styles.insert(tag.to_string(), preset);
            save_custom_composer_styles(&styles);
        }
        if tag == "p" {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_font_family(&font_family);
                settings.set_mail_font_size(font_size_px);
            }
        }
        self.apply_custom_styles_to_dom();
    }

    pub fn reset_current_style_preset(&self) {
        let tag = match self
            .imp()
            .dropdown_format
            .borrow()
            .as_ref()
            .map(|dd| dd.selected())
            .unwrap_or(0)
        {
            1 => "h1",
            2 => "h2",
            3 => "h3",
            4 => "pre",
            _ => "p",
        };
        {
            let mut styles = self.imp().custom_styles.borrow_mut();
            styles.remove(tag);
            save_custom_composer_styles(&styles);
        }
        self.apply_custom_styles_to_dom();
    }

    pub fn set_highlight_color(&self, color: &str) {
        if self.plain_text_mode() {
            return;
        }
        let target = if color.is_empty() { "transparent" } else { color };
        let Ok(encoded) = serde_json::to_string(target) else {
            return;
        };
        self.evaluate_or_queue(format!(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('styleWithCSS', false, true); document.execCommand('hiliteColor', false, {encoded}); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();"
        ));
    }

    pub fn set_text_alignment(&self, command: &str) {
        if self.plain_text_mode() {
            return;
        }
        if !matches!(
            command,
            "justifyLeft" | "justifyCenter" | "justifyRight" | "justifyFull" | "indent" | "outdent"
        ) {
            return;
        }
        let script = if command == "indent" {
            "if (typeof window.meridianIndent === 'function') { window.meridianIndent(); } else { document.execCommand('indent', false, null); }".to_string()
        } else if command == "outdent" {
            "if (typeof window.meridianOutdent === 'function') { window.meridianOutdent(); } else { document.execCommand('outdent', false, null); }".to_string()
        } else {
            format!(
                "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('{command}', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();"
            )
        };
        self.evaluate_or_queue(script);
    }

    pub fn insert_table(&self, rows: u32, cols: u32) {
        if self.plain_text_mode() {
            return;
        }
        let r = rows.clamp(1, 20);
        let c = cols.clamp(1, 12);
        self.evaluate_or_queue(format!(
            "if (typeof window.meridianInsertTable === 'function') {{ window.meridianInsertTable({r}, {c}); }}"
        ));
    }

    pub fn modify_table(&self, action: &str) {
        if self.plain_text_mode() {
            return;
        }
        if !matches!(action, "addRow" | "addCol" | "delRow" | "delCol" | "delTable") {
            return;
        }
        self.evaluate_or_queue(format!(
            "if (typeof window.meridianModifyTable === 'function') {{ window.meridianModifyTable('{action}'); }}"
        ));
    }

    pub fn forward_delete(&self) {
        self.evaluate_or_queue(
            "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('forwardDelete', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string(),
        );
    }

    pub(crate) fn apply_selection_state_json(&self, raw: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
            return;
        };
        let is_bold = parsed.get("bold").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_italic = parsed.get("italic").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_underline = parsed.get("underline").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_strike = parsed
            .get("strikethrough")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_ul = parsed.get("ul").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_ol = parsed.get("ol").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_quote = parsed
            .get("blockquote")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let set_active_btn = |btn_opt: &Option<gtk::Button>, active: bool| {
            if let Some(btn) = btn_opt.as_ref() {
                if active {
                    btn.add_css_class("suggested-action");
                } else {
                    btn.remove_css_class("suggested-action");
                }
            }
        };
        set_active_btn(&self.imp().btn_bold.borrow(), is_bold);
        set_active_btn(&self.imp().btn_italic.borrow(), is_italic);
        set_active_btn(&self.imp().btn_underline.borrow(), is_underline);
        set_active_btn(&self.imp().btn_strikethrough.borrow(), is_strike);
        set_active_btn(&self.imp().btn_list.borrow(), is_ul);
        set_active_btn(&self.imp().btn_ordered_list.borrow(), is_ol);
        set_active_btn(&self.imp().btn_blockquote.borrow(), is_quote);

        self.imp().suppress_toolbar_sync.set(true);
        if let Some(tag) = parsed.get("block_tag").and_then(|v| v.as_str()) {
            let fmt_idx = match tag {
                "h1" => 1,
                "h2" => 2,
                "h3" => 3,
                "pre" => 4,
                _ => 0,
            };
            if let Some(dd) = self.imp().dropdown_format.borrow().as_ref() {
                if dd.selected() != fmt_idx {
                    dd.set_selected(fmt_idx);
                }
            }
            if let Some(btn_typo) = self.imp().btn_typography.borrow().as_ref() {
                if fmt_idx != 0 {
                    btn_typo.add_css_class("suggested-action");
                } else {
                    btn_typo.remove_css_class("suggested-action");
                }
            }
        }
        if let Some(ff) = parsed.get("font_family").and_then(|v| v.as_str()) {
            if !ff.is_empty() {
                let ff_idx = font_family_index_for_css(ff);
                if let Some(dd) = self.imp().dropdown_font_family.borrow().as_ref() {
                    if dd.selected() != ff_idx {
                        dd.set_selected(ff_idx);
                    }
                }
            }
        }
        if let Some(fs) = parsed.get("font_size").and_then(|v| v.as_u64()) {
            let fs_idx = font_size_index_for_px(fs as u32);
            if let Some(dd) = self.imp().dropdown_font_size.borrow().as_ref() {
                if dd.selected() != fs_idx {
                    dd.set_selected(fs_idx);
                }
            }
        }
        self.imp().suppress_toolbar_sync.set(false);
        self.sync_typography_popover_ui();
    }

    pub fn popdown_toolbar_popovers(&self) {
        for pop_cell in [
            &self.imp().typography_popover,
            &self.imp().color_popover,
            &self.imp().align_popover,
            &self.imp().link_popover,
            &self.imp().table_popover,
        ] {
            if let Some(pop) = pop_cell.borrow().as_ref() {
                if pop.is_visible() {
                    pop.popdown();
                }
            }
        }
    }

    pub(crate) fn sync_typography_popover_ui(&self) {
        let fmt_idx = self
            .imp()
            .dropdown_format
            .borrow()
            .as_ref()
            .map(|dd| dd.selected() as usize)
            .unwrap_or(0);
        for (idx, btn) in self.imp().style_buttons.borrow().iter().enumerate() {
            if idx == fmt_idx {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }

        let ff_idx = self
            .imp()
            .dropdown_font_family
            .borrow()
            .as_ref()
            .map(|dd| dd.selected() as usize)
            .unwrap_or(0);
        for (idx, btn) in self.imp().font_family_buttons.borrow().iter().enumerate() {
            if idx == ff_idx {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }

        let fs_idx = self
            .imp()
            .dropdown_font_size
            .borrow()
            .as_ref()
            .map(|dd| dd.selected())
            .unwrap_or(0);
        let current_px = {
            let px = font_size_px_for_index(fs_idx);
            if px == 0 {
                AppSettings::try_new()
                    .map(|s| s.mail_font_size())
                    .unwrap_or(14)
            } else {
                px
            }
        };
        if let Some(lbl) = self.imp().font_size_label.borrow().as_ref() {
            lbl.set_label(&format!("{current_px} px"));
        }
        for (px, btn) in self.imp().font_size_buttons.borrow().iter() {
            if *px == current_px {
                btn.add_css_class("active");
            } else {
                btn.remove_css_class("active");
            }
        }
    }

    pub fn spell_checking_enabled(&self) -> bool {
        self.imp().spell_checking_enabled.get()
    }

    pub fn auto_format_enabled(&self) -> bool {
        self.imp().auto_format_enabled.get()
    }

    pub fn set_auto_format_enabled(&self, enabled: bool) {
        self.imp().auto_format_enabled.set(enabled);
        self.evaluate_or_queue(format!(
            "window.__meridianAutoFormatEnabled = {};",
            if enabled { "true" } else { "false" }
        ));
    }

    pub fn set_block_format_index(&self, idx: u32) {
        if self.plain_text_mode() {
            return;
        }
        let clamped = idx.min(4);
        if let Some(dd) = self.imp().dropdown_format.borrow().as_ref() {
            dd.set_selected(clamped);
        }
    }

    pub fn set_spell_checking_enabled(&self, enabled: bool) {
        self.imp().spell_checking_enabled.set(enabled);
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_enabled(enabled);
        }
    }

    pub fn spell_checking_languages(&self) -> Vec<String> {
        self.imp().spell_checking_languages.borrow().clone()
    }

    pub fn set_spell_checking_languages(&self, languages: &[&str]) {
        self.imp()
            .spell_checking_languages
            .replace(languages.iter().map(|s| s.to_string()).collect());
        if let Some(web_context) = self.imp().web_context.borrow().as_ref() {
            web_context.set_spell_checking_languages(languages);
        }
    }

    pub fn apply_spellcheck_settings(&self, settings: &AppSettings) {
        self.set_spell_checking_enabled(settings.composer_inline_spelling());
        let raw = settings.spellcheck_languages();
        let languages: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        self.set_spell_checking_languages(&languages);
    }

    pub fn plain_text_mode(&self) -> bool {
        self.imp().plain_text_mode.get()
    }

    pub fn connect_plain_text_mode_changed<F: Fn(bool) + 'static>(&self, f: F) {
        self.imp()
            .plain_text_mode_callbacks
            .borrow_mut()
            .0
            .push(std::rc::Rc::new(f));
    }

    /// Switch between rich HTML and literal text while preserving the current
    /// visible content.  The conversion is performed inside the DOM so user
    /// text is never interpolated into JavaScript source.
    pub fn set_plain_text_mode(&self, plain_text: bool) {
        if self.plain_text_mode() == plain_text {
            return;
        }
        self.imp().plain_text_mode.set(plain_text);
        if let Some(toolbar) = self.imp().format_toolbar.borrow().as_ref() {
            toolbar.set_sensitive(!plain_text);
        }
        if let Some(btn_html) = self.imp().mode_html_btn.borrow().as_ref() {
            if btn_html.is_active() != (!plain_text) {
                btn_html.set_active(!plain_text);
            }
        }
        if let Some(btn_txt) = self.imp().mode_txt_btn.borrow().as_ref() {
            if btn_txt.is_active() != plain_text {
                btn_txt.set_active(plain_text);
            }
        }
        let script = if plain_text {
            "document.body.textContent = document.body.innerText; document.body.style.whiteSpace = 'pre-wrap';"
                .to_string()
        } else {
            concat!(
                "const text = document.body.innerText;",
                "document.body.innerHTML = '';",
                "const lines = text.split(/\\n/);",
                "for (const line of lines) { const p = document.createElement('p'); ",
                "p.textContent = line || '\\u00a0'; document.body.appendChild(p); }",
                "document.body.style.whiteSpace = 'normal';"
            )
            .to_string()
        };
        self.evaluate_or_queue(script);
        self.evaluate_or_queue(format!("window.__plainTextMode = {};", plain_text));
        let callbacks = self.imp().plain_text_mode_callbacks.borrow().0.clone();
        for cb in callbacks {
            cb(plain_text);
        }
    }

    pub fn is_format_toolbar_visible(&self) -> bool {
        self.imp()
            .format_toolbar_revealer
            .borrow()
            .as_ref()
            .map(|r| r.reveals_child())
            .unwrap_or(false)
    }

    pub fn set_format_toolbar_visible(&self, visible: bool) {
        if let Some(revealer) = self.imp().format_toolbar_revealer.borrow().as_ref() {
            revealer.set_reveal_child(visible);
        }
    }

    pub fn toggle_format_toolbar(&self) -> bool {
        let new_state = !self.is_format_toolbar_visible();
        self.set_format_toolbar_visible(new_state);
        new_state
    }

    pub fn is_toolbar_scrollable(&self) -> bool {
        if let Some(toolbar_scroll) = self.imp().toolbar_scroll.borrow().as_ref() {
            toolbar_scroll.parent().is_some()
        } else {
            false
        }
    }

    pub fn set_toolbar_scrollable(&self, scrollable: bool) {
        let Some(toolbar) = self.imp().format_toolbar.borrow().as_ref().cloned() else {
            return;
        };
        let Some(toolbar_scroll) = self.imp().toolbar_scroll.borrow().as_ref().cloned() else {
            return;
        };
        let Some(revealer) = self.imp().format_toolbar_revealer.borrow().as_ref().cloned() else {
            return;
        };
        let Some(container) = revealer.child().and_downcast::<gtk::Box>() else {
            return;
        };

        if scrollable {
            if toolbar.parent().as_ref() == Some(container.upcast_ref()) {
                container.remove(&toolbar);
                toolbar_scroll.set_child(Some(&toolbar));
                container.append(&toolbar_scroll);
            }
        } else {
            if toolbar_scroll.parent().as_ref() == Some(container.upcast_ref()) {
                toolbar_scroll.set_child(None::<&gtk::Widget>);
                container.remove(&toolbar_scroll);
                container.append(&toolbar);
            }
        }
    }

    pub fn is_html_mode(&self) -> bool {
        !self.plain_text_mode()
    }

    pub fn set_html_mode(&self, html_mode: bool) {
        self.set_plain_text_mode(!html_mode);
    }

    pub fn format_toggle_button(&self) -> gtk::ToggleButton {
        self.imp()
            .mode_html_btn
            .borrow()
            .as_ref()
            .cloned()
            .expect("mode_html_btn must be initialized")
    }

    pub fn mode_switcher(&self) -> gtk::Box {
        self.imp()
            .mode_switcher
            .borrow()
            .as_ref()
            .cloned()
            .expect("mode_switcher must be initialized")
    }

    pub fn top_bar(&self) -> Option<gtk::Box> {
        self.imp().top_bar.borrow().clone()
    }

    pub fn prepend_top_bar_widget(&self, widget: &impl IsA<gtk::Widget>) {
        if let Some(top_bar) = self.imp().top_bar.borrow().as_ref() {
            top_bar.prepend(widget);
        }
    }

    pub fn prepend_toolbar_widget(&self, widget: &impl IsA<gtk::Widget>) {
        self.prepend_top_bar_widget(widget);
    }

    pub fn set_canvas_min_height(&self, min_height: i32) {
        if let Some(scroll) = self.imp().web_view_scroll.borrow().as_ref() {
            scroll.set_min_content_height(min_height);
        }
    }

    pub fn connect_content_changed<F: Fn(&str, &str) + 'static>(&self, callback: F) {
        self.imp()
            .content_changed_callbacks
            .borrow_mut()
            .0
            .push(std::rc::Rc::new(callback));
    }

    fn handle_content_changed_script_message(&self, raw: &str) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
            let html = parsed.get("html").and_then(|v| v.as_str()).unwrap_or("");
            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let callbacks = self.imp().content_changed_callbacks.borrow().0.clone();
            for cb in callbacks {
                cb(html, text);
            }
        }
    }

    pub fn set_canvas_card_mode(&self, enabled: bool) {
        self.imp().canvas_card_mode.set(enabled);
        if let Some(scroll) = self.imp().web_view_scroll.borrow().as_ref() {
            if enabled {
                scroll.add_css_class("card");
                scroll.add_css_class("view");
                scroll.set_overflow(gtk::Overflow::Hidden);
                scroll.set_margin_top(6);
            } else {
                scroll.remove_css_class("card");
                scroll.remove_css_class("view");
                scroll.set_overflow(gtk::Overflow::Visible);
                scroll.set_margin_top(0);
            }
        }
        if let Some(tb) = self.imp().format_toolbar.borrow().as_ref() {
            if enabled {
                tb.set_margin_start(0);
                tb.set_margin_end(0);
                tb.set_margin_top(0);
                tb.set_margin_bottom(2);
            } else {
                tb.set_margin_start(16);
                tb.set_margin_end(16);
                tb.set_margin_top(6);
                tb.set_margin_bottom(6);
            }
        }
        if let Some(ts) = self.imp().toolbar_scroll.borrow().as_ref() {
            if enabled {
                ts.set_propagate_natural_width(true);
            } else {
                ts.set_propagate_natural_width(false);
            }
        }
        if let Some(top_bar) = self.imp().top_bar.borrow().as_ref() {
            if enabled {
                top_bar.set_halign(gtk::Align::Start);
                top_bar.set_hexpand(false);
            } else {
                top_bar.set_halign(gtk::Align::Fill);
                top_bar.set_hexpand(true);
            }
        }
        if let Some(revealer) = self.imp().format_toolbar_revealer.borrow().as_ref() {
            if enabled {
                revealer.set_halign(gtk::Align::Start);
                revealer.set_hexpand(false);
            } else {
                revealer.set_halign(gtk::Align::Fill);
                revealer.set_hexpand(true);
            }
        }
        if let Some(ms) = self.imp().mode_switcher.borrow().as_ref() {
            if enabled {
                ms.set_margin_start(6);
                ms.set_margin_end(0);
                ms.set_margin_top(0);
                ms.set_margin_bottom(2);
            } else {
                ms.set_margin_start(8);
                ms.set_margin_end(16);
                ms.set_margin_top(6);
                ms.set_margin_bottom(6);
            }
        }
    }

    pub fn canvas_card_mode(&self) -> bool {
        self.imp().canvas_card_mode.get()
    }

    pub fn clear_selection(&self) {
        self.evaluate_or_queue(
            "const sel = window.getSelection(); if (sel && !sel.isCollapsed) { sel.collapseToStart(); }".to_string(),
        );
    }

    pub fn web_view(&self) -> Option<webkit6::WebView> {
        self.imp().web_view.borrow().clone()
    }

    /// Focus the editor WebView and place the caret at the beginning of the first line.
    pub fn focus_start(&self) {
        self.imp().focus_on_ready.set(true);
        if let Some(web_view) = self.imp().web_view.borrow().as_ref() {
            web_view.grab_focus();
        }
        self.evaluate_or_queue(
            "try { const first = document.body.firstElementChild || document.body.firstChild; if (first) { const range = document.createRange(); range.setStart(first, 0); range.collapse(true); const sel = window.getSelection(); if (sel) { sel.removeAllRanges(); sel.addRange(range); } } document.body.focus(); window.scrollTo(0, 0); } catch (e) {}".to_string(),
        );
    }

    fn evaluate_or_queue(&self, script: String) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        if !self.imp().web_view_ready.get() {
            self.imp().pending_scripts.borrow_mut().push(script);
            return;
        }
        web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
    }

    fn flush_pending_scripts(&self) {
        let Some(web_view) = self.imp().web_view.borrow().as_ref().cloned() else {
            return;
        };
        self.imp().web_view_ready.set(true);
        let scripts = std::mem::take(&mut *self.imp().pending_scripts.borrow_mut());
        for script in scripts {
            web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        }
        if self.imp().focus_on_ready.get() {
            web_view.grab_focus();
            web_view.evaluate_javascript(
                "try { const first = document.body.firstElementChild || document.body.firstChild; if (first) { const range = document.createRange(); range.setStart(first, 0); range.collapse(true); const sel = window.getSelection(); if (sel) { sel.removeAllRanges(); sel.addRange(range); } } document.body.focus(); window.scrollTo(0, 0); } catch (e) {}",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    }

    fn build_ui(&self) {
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        // WYSIWYG Toolbar
        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(2)
            .margin_start(16)
            .margin_end(16)
            .margin_top(6)
            .margin_bottom(6)
            .css_classes(["composer-format-toolbar"])
            .build();

        let btn_bold = gtk::Button::builder()
            .icon_name("meridian-format-bold-symbolic")
            .tooltip_text(gettext("Bold (Ctrl+B)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_bold, &gettext("Bold"));

        let btn_italic = gtk::Button::builder()
            .icon_name("meridian-format-italic-symbolic")
            .tooltip_text(gettext("Italic (Ctrl+I)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_italic, &gettext("Italic"));

        let btn_underline = gtk::Button::builder()
            .icon_name("meridian-format-underline-symbolic")
            .tooltip_text(gettext("Underline (Ctrl+U)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_underline, &gettext("Underline"));

        let btn_strikethrough = gtk::Button::builder()
            .icon_name("meridian-format-strikethrough-symbolic")
            .tooltip_text(gettext("Strikethrough (Ctrl+Shift+X)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_strikethrough, &gettext("Strikethrough"));

        let btn_clear = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text(gettext("Clear formatting (Ctrl+\\)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_clear, &gettext("Clear formatting"));

        let format_labels = [
            gettext("Normal"),
            gettext("Heading 1"),
            gettext("Heading 2"),
            gettext("Heading 3"),
            gettext("Code"),
        ];
        let format_strs: Vec<&str> = format_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_format = gtk::DropDown::from_strings(&format_strs);
        dropdown_format.set_valign(gtk::Align::Center);
        dropdown_format.set_tooltip_text(Some(&gettext("Paragraph style")));
        configure_fixed_width_dropdown(&dropdown_format, DROPDOWN_FORMAT_WIDTH, 7);
        configure_style_dropdown_list_preview(&dropdown_format);
        set_accessible_label(&dropdown_format, &gettext("Paragraph format"));

        let font_family_labels: Vec<String> = COMPOSER_FONT_FAMILIES
            .iter()
            .map(|(label, _)| {
                if *label == "System Default" {
                    gettext("Default")
                } else {
                    (*label).to_string()
                }
            })
            .collect();
        let font_family_strs: Vec<&str> = font_family_labels.iter().map(|s| s.as_str()).collect();
        let dropdown_font_family = gtk::DropDown::from_strings(&font_family_strs);
        dropdown_font_family.set_valign(gtk::Align::Center);
        dropdown_font_family.set_tooltip_text(Some(&gettext("Font Family")));
        configure_fixed_width_dropdown(&dropdown_font_family, DROPDOWN_FONT_FAMILY_WIDTH, 9);
        configure_font_dropdown_list_preview(&dropdown_font_family);
        set_accessible_label(&dropdown_font_family, &gettext("Font family"));
        if let Some(settings) = AppSettings::try_new() {
            dropdown_font_family.set_selected(font_family_index_for_css(&settings.mail_font_family()));
        }

        let font_size_strings: Vec<String> =
            COMPOSER_FONT_SIZES.iter().map(|px| px.to_string()).collect();
        let font_size_labels: Vec<&str> = font_size_strings.iter().map(String::as_str).collect();
        let dropdown_font_size = gtk::DropDown::from_strings(&font_size_labels);
        dropdown_font_size.set_valign(gtk::Align::Center);
        dropdown_font_size.set_tooltip_text(Some(&gettext("Font Size")));
        configure_fixed_width_dropdown(&dropdown_font_size, DROPDOWN_FONT_SIZE_WIDTH, 2);
        set_accessible_label(&dropdown_font_size, &gettext("Font size"));
        if let Some(settings) = AppSettings::try_new() {
            dropdown_font_size.set_selected(font_size_index_for_px(settings.mail_font_size()));
        }

        // Load user-customized block styles (Normal, H1, H2, H3, Preformatted)
        let initial_custom_styles = load_custom_composer_styles();
        self.imp().custom_styles.replace(initial_custom_styles);

        let typography_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let btn_save_style = gtk::Button::builder()
            .label(gettext("Update style to match selection"))
            .tooltip_text(gettext(
                "Save current font, size, and formatting as default for the selected style",
            ))
            .css_classes(["flat", "caption"])
            .build();
        set_accessible_label(&btn_save_style, &gettext("Update style to match selection"));
        let btn_reset_style = gtk::Button::builder()
            .label(gettext("Reset style to default"))
            .tooltip_text(gettext("Reset selected style to default"))
            .css_classes(["flat", "caption"])
            .build();
        set_accessible_label(&btn_reset_style, &gettext("Reset style to default"));
        typography_box.append(&btn_save_style);
        typography_box.append(&btn_reset_style);

        let typography_popover = gtk::Popover::builder()
            .child(&typography_box)
            .autohide(true)
            .build();
        {
            let editor_weak = self.downgrade();
            let pop_weak = typography_popover.downgrade();
            btn_save_style.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.save_current_style_preset();
                }
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
            });
        }
        {
            let editor_weak = self.downgrade();
            let pop_weak = typography_popover.downgrade();
            btn_reset_style.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.reset_current_style_preset();
                }
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
            });
        }
        self.imp().btn_save_style.replace(Some(btn_save_style));
        self.imp().btn_reset_style.replace(Some(btn_reset_style));
        self.imp()
            .typography_popover
            .replace(Some(typography_popover.clone()));

        let btn_typography = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(gettext("Style defaults (Update or reset current style)"))
            .popover(&typography_popover)
            .css_classes(["flat", "composer-style-menu-btn"])
            .build();
        set_accessible_label(&btn_typography, &gettext("Style defaults"));

        // Minimalist Native Libadwaita Color Popover (Circular GNOME HIG Swatches, Single-View)
        let initial_recent_colors = load_recent_composer_colors();
        self.imp().recent_colors.replace(initial_recent_colors);

        let color_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(8)
            .build();
        let color_popover = gtk::Popover::builder()
            .child(&color_popover_box)
            .autohide(true)
            .build();

        color_popover_box.append(
            &gtk::Label::builder()
                .label(gettext("Text color"))
                .halign(gtk::Align::Start)
                .margin_start(2)
                .css_classes(["caption-heading", "dim-label"])
                .build(),
        );

        let text_color_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .build();

        let btn_default_color = build_cairo_reset_swatch_button(&gettext("Default color"));
        {
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            let row_clone = text_color_row.clone();
            btn_default_color.connect_clicked(move |btn| {
                select_swatch_in_row(&row_clone, btn);
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_color("");
                }
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
        }
        text_color_row.append(&btn_default_color);

        let gnome_text_colors: &[(&str, &str)] = &[
            ("#3584e4", "Blue"),
            ("#2190a4", "Teal"),
            ("#3a944a", "Green"),
            ("#c88800", "Yellow"),
            ("#ed5b00", "Orange"),
            ("#e62d42", "Red"),
            ("#9141ac", "Purple"),
        ];
        for (hex, name) in gnome_text_colors {
            let swatch = build_cairo_color_swatch_button(hex, &gettext(*name));
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            let row_clone = text_color_row.clone();
            let hex_str = (*hex).to_string();
            swatch.connect_clicked(move |btn| {
                select_swatch_in_row(&row_clone, btn);
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_color(&hex_str);
                }
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
            text_color_row.append(&swatch);
        }

        let btn_custom_color = build_cairo_custom_swatch_button(&gettext("Custom color…"));
        {
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            btn_custom_color.connect_clicked(move |_| {
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
                let Some(editor) = editor_weak.upgrade() else {
                    return;
                };
                let dialog = gtk::ColorDialog::builder()
                    .title(gettext("Choose Text Color"))
                    .modal(true)
                    .with_alpha(false)
                    .build();
                let parent_win = editor
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let editor_weak_inner = editor.downgrade();
                dialog.choose_rgba(
                    parent_win.as_ref(),
                    None::<&gtk::gdk::RGBA>,
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok(rgba) = res {
                            let r = (rgba.red().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let g = (rgba.green().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let b = (rgba.blue().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let hex = format!("#{r:02x}{g:02x}{b:02x}");
                            if let Some(ed) = editor_weak_inner.upgrade() {
                                ed.set_text_color(&hex);
                            }
                        }
                    },
                );
            });
        }
        text_color_row.append(&btn_custom_color);
        color_popover_box.append(&text_color_row);

        let recent_color_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        color_popover_box.append(&recent_color_row);
        self.imp()
            .recent_color_row
            .replace(Some(recent_color_row.clone()));

        color_popover_box.append(
            &gtk::Label::builder()
                .label(gettext("Background color"))
                .halign(gtk::Align::Start)
                .margin_start(2)
                .margin_top(4)
                .css_classes(["caption-heading", "dim-label"])
                .build(),
        );

        let highlight_color_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .build();

        let btn_no_highlight = build_cairo_reset_swatch_button(&gettext("No background color"));
        {
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            let row_clone = highlight_color_row.clone();
            btn_no_highlight.connect_clicked(move |btn| {
                select_swatch_in_row(&row_clone, btn);
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_highlight_color("transparent");
                }
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
        }
        highlight_color_row.append(&btn_no_highlight);

        let highlight_colors: &[(&str, &str)] = &[
            ("rgba(153, 193, 241, 0.70)", "Blue"),
            ("rgba(143, 240, 212, 0.70)", "Teal"),
            ("rgba(143, 240, 164, 0.75)", "Green"),
            ("rgba(249, 240, 107, 0.80)", "Yellow"),
            ("rgba(255, 190, 111, 0.75)", "Orange"),
            ("rgba(246, 97, 81, 0.55)", "Pink"),
            ("rgba(220, 138, 221, 0.65)", "Purple"),
        ];
        for (rgba, name) in highlight_colors {
            let swatch = build_cairo_color_swatch_button(rgba, &gettext(*name));
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            let row_clone = highlight_color_row.clone();
            let rgba_str = (*rgba).to_string();
            swatch.connect_clicked(move |btn| {
                select_swatch_in_row(&row_clone, btn);
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_highlight_color(&rgba_str);
                }
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
            highlight_color_row.append(&swatch);
        }

        let btn_custom_bg_color =
            build_cairo_custom_swatch_button(&gettext("Custom background color…"));
        {
            let editor_weak = self.downgrade();
            let popover_weak = color_popover.downgrade();
            btn_custom_bg_color.connect_clicked(move |btn| {
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
                let Some(editor) = editor_weak.upgrade() else {
                    return;
                };
                let window = btn
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let dialog = gtk::ColorDialog::builder()
                    .title(gettext("Select Background Color"))
                    .modal(true)
                    .with_alpha(true)
                    .build();
                let ed_weak = editor.downgrade();
                dialog.choose_rgba(
                    window.as_ref(),
                    None::<&gtk::gdk::RGBA>,
                    None::<&gio::Cancellable>,
                    move |res| {
                        if let Ok(rgba) = res {
                            let r = (rgba.red().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let g = (rgba.green().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let b = (rgba.blue().clamp(0.0, 1.0) * 255.0).round() as u8;
                            let a = rgba.alpha().clamp(0.15, 1.0);
                            let css_color = if (a - 1.0).abs() < 0.01 {
                                format!("#{:02x}{:02x}{:02x}", r, g, b)
                            } else {
                                format!("rgba({}, {}, {}, {:.2})", r, g, b, a)
                            };
                            if let Some(ed) = ed_weak.upgrade() {
                                ed.set_highlight_color(&css_color);
                            }
                        }
                    },
                );
            });
        }
        highlight_color_row.append(&btn_custom_bg_color);
        color_popover_box.append(&highlight_color_row);

        self.imp().color_popover.replace(Some(color_popover.clone()));
        self.rebuild_recent_colors_row();

        // Standard "A" with live color bar underneath for immediate recognition
        let color_btn_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(1)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let color_btn_label = gtk::Label::builder()
            .label("A")
            .css_classes(["composer-color-btn-glyph"])
            .build();
        let color_indicator_bar = gtk::DrawingArea::builder()
            .content_width(14)
            .content_height(3)
            .halign(gtk::Align::Center)
            .build();
        {
            let editor_weak = self.downgrade();
            color_indicator_bar.set_draw_func(move |_, cr, w, h| {
                let hex = editor_weak
                    .upgrade()
                    .map(|ed| ed.imp().last_text_color.borrow().clone())
                    .unwrap_or_default();
                let color_str = if hex.trim().is_empty() {
                    "#3584e4"
                } else {
                    hex.trim()
                };
                if let Ok(rgba) = gtk::gdk::RGBA::parse(color_str) {
                    cr.set_source_rgba(
                        rgba.red() as f64,
                        rgba.green() as f64,
                        rgba.blue() as f64,
                        rgba.alpha() as f64,
                    );
                    draw_cairo_rounded_rect(cr, 0.0, 0.0, w as f64, h as f64, 1.5);
                    let _ = cr.fill();
                }
            });
        }
        self.imp()
            .color_indicator_bar
            .replace(Some(color_indicator_bar.clone()));
        color_btn_content.append(&color_btn_label);
        color_btn_content.append(&color_indicator_bar);

        let btn_color = gtk::MenuButton::builder()
            .child(&color_btn_content)
            .tooltip_text(gettext("Text & background color"))
            .popover(&color_popover)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_color, &gettext("Text & background color"));

        // Compact Alignment & Indentation Popover
        let align_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let align_popover = gtk::Popover::builder()
            .child(&align_popover_box)
            .autohide(true)
            .build();
        self.imp().align_popover.replace(Some(align_popover.clone()));

        let align_group = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["linked"])
            .build();
        for (icon, tip, cmd) in [
            ("format-justify-left-symbolic", "Align left", "justifyLeft"),
            ("format-justify-center-symbolic", "Align center", "justifyCenter"),
            ("format-justify-right-symbolic", "Align right", "justifyRight"),
            ("format-justify-fill-symbolic", "Justify", "justifyFull"),
        ] {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(gettext(tip))
                .css_classes(["flat"])
                .build();
            set_accessible_label(&b, &gettext(tip));
            let editor_weak = self.downgrade();
            let popover_weak = align_popover.downgrade();
            let cmd_str = cmd.to_string();
            b.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_alignment(&cmd_str);
                }
                if let Some(p) = popover_weak.upgrade() {
                    p.popdown();
                }
            });
            align_group.append(&b);
        }

        align_popover_box.append(&align_group);

        let btn_align = gtk::MenuButton::builder()
            .icon_name("format-justify-left-symbolic")
            .tooltip_text(gettext("Alignment"))
            .popover(&align_popover)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_align, &gettext("Alignment"));

        let btn_outdent = gtk::Button::builder()
            .icon_name("format-indent-less-symbolic")
            .tooltip_text(gettext("Decrease indent (Shift+Tab)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_outdent, &gettext("Decrease indent"));
        {
            let editor_weak = self.downgrade();
            btn_outdent.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_alignment("outdent");
                }
            });
        }

        let btn_indent = gtk::Button::builder()
            .icon_name("format-indent-more-symbolic")
            .tooltip_text(gettext("Increase indent (Tab)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_indent, &gettext("Increase indent"));
        {
            let editor_weak = self.downgrade();
            btn_indent.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_text_alignment("indent");
                }
            });
        }

        let btn_list = gtk::Button::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(gettext("Bullet list"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_list, &gettext("Bullet list"));

        let btn_ordered_list = gtk::Button::builder()
            .icon_name("view-list-ordered-symbolic")
            .tooltip_text(gettext("Numbered list"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_ordered_list, &gettext("Numbered list"));

        let btn_blockquote = gtk::Button::builder()
            .icon_name("meridian-format-quote-symbolic")
            .tooltip_text(gettext("Block quote"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_blockquote, &gettext("Block quote"));

        let link_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(8)
            .width_request(260)
            .build();
        link_popover_box.append(
            &gtk::Label::builder()
                .label(gettext("Insert hyperlink"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let entry_url = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .build();
        let entry_label = gtk::Entry::builder()
            .placeholder_text(gettext("Link text (optional)"))
            .build();
        let btn_insert_link_confirm = gtk::Button::builder()
            .label(gettext("Insert"))
            .css_classes(["suggested-action"])
            .build();
        link_popover_box.append(&entry_url);
        link_popover_box.append(&entry_label);
        link_popover_box.append(&btn_insert_link_confirm);

        let btn_confirm_url = btn_insert_link_confirm.clone();
        entry_url.connect_activate(move |_| {
            btn_confirm_url.emit_clicked();
        });
        let btn_confirm_label = btn_insert_link_confirm.clone();
        entry_label.connect_activate(move |_| {
            btn_confirm_label.emit_clicked();
        });

        let link_popover = gtk::Popover::builder().child(&link_popover_box).build();
        let btn_link = gtk::MenuButton::builder()
            .icon_name("meridian-insert-link-symbolic")
            .tooltip_text(gettext("Insert hyperlink (Ctrl+K)"))
            .popover(&link_popover)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_link, &gettext("Insert hyperlink"));

        let btn_rule = gtk::Button::builder()
            .icon_name("meridian-horizontal-rule-symbolic")
            .tooltip_text(gettext("Insert horizontal rule"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_rule, &gettext("Insert horizontal rule"));

        let btn_image = gtk::Button::builder()
            .icon_name("meridian-insert-image-symbolic")
            .tooltip_text(gettext("Insert image"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_image, &gettext("Insert image"));

        // Minimalist Table Picker Popover (5x5 Grid; Row/Col/+/- controls live interactively on the table in the composer)
        let table_popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let table_popover = gtk::Popover::builder()
            .child(&table_popover_box)
            .autohide(true)
            .build();

        let table_size_label = gtk::Label::builder()
            .label("3 × 3")
            .halign(gtk::Align::Center)
            .css_classes(["caption-heading", "dim-label"])
            .build();
        table_popover_box.append(&table_size_label);

        let table_grid = gtk::Grid::builder()
            .row_spacing(3)
            .column_spacing(3)
            .margin_top(2)
            .margin_bottom(2)
            .margin_start(2)
            .margin_end(2)
            .halign(gtk::Align::Center)
            .build();

        let mut grid_cells: Vec<(usize, usize, gtk::Button)> = Vec::with_capacity(25);
        for r in 0..5usize {
            for c in 0..5usize {
                let cell_btn = gtk::Button::builder()
                    .tooltip_text(format!("{} × {}", r + 1, c + 1))
                    .css_classes(["flat", "composer-table-grid-cell"])
                    .build();
                if r < 3 && c < 3 {
                    cell_btn.add_css_class("active");
                }
                table_grid.attach(&cell_btn, c as i32, r as i32, 1, 1);
                grid_cells.push((r, c, cell_btn));
            }
        }
        let shared_cells = std::rc::Rc::new(grid_cells);
        for (r, c, cell_btn) in shared_cells.iter() {
            let r_val = *r;
            let c_val = *c;
            let cells_for_hover = shared_cells.clone();
            let lbl_for_hover = table_size_label.clone();
            let motion = gtk::EventControllerMotion::new();
            motion.connect_enter(move |_, _, _| {
                lbl_for_hover.set_label(&format!("{} × {}", r_val + 1, c_val + 1));
                for (cr, cc, b) in cells_for_hover.iter() {
                    if *cr <= r_val && *cc <= c_val {
                        b.add_css_class("active");
                    } else {
                        b.remove_css_class("active");
                    }
                }
            });
            cell_btn.add_controller(motion);

            let editor_weak = self.downgrade();
            let pop_weak = table_popover.downgrade();
            cell_btn.connect_clicked(move |_| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.insert_table((r_val + 1) as u32, (c_val + 1) as u32);
                }
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
            });
        }
        table_popover_box.append(&table_grid);

        let btn_table = gtk::MenuButton::builder()
            .icon_name("meridian-insert-table-symbolic")
            .tooltip_text(gettext("Insert table"))
            .popover(&table_popover)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_table, &gettext("Insert table"));

        let btn_undo = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text(gettext("Undo (Ctrl+Z)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_undo, &gettext("Undo"));

        let btn_redo = gtk::Button::builder()
            .icon_name("edit-redo-symbolic")
            .tooltip_text(gettext("Redo (Ctrl+Shift+Z)"))
            .css_classes(["flat"])
            .build();
        set_accessible_label(&btn_redo, &gettext("Redo"));

        toolbar.append(&btn_undo);
        toolbar.append(&btn_redo);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&dropdown_format);
        toolbar.append(&dropdown_font_family);
        toolbar.append(&dropdown_font_size);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_bold);
        toolbar.append(&btn_italic);
        toolbar.append(&btn_underline);
        toolbar.append(&btn_strikethrough);
        toolbar.append(&btn_color);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_align);
        toolbar.append(&btn_list);
        toolbar.append(&btn_ordered_list);
        toolbar.append(&btn_outdent);
        toolbar.append(&btn_indent);
        toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        toolbar.append(&btn_blockquote);
        toolbar.append(&btn_link);
        toolbar.append(&btn_image);
        toolbar.append(&btn_table);
        toolbar.append(&btn_rule);

        self.imp().btn_typography.replace(Some(btn_typography));
        self.imp().dropdown_format.replace(Some(dropdown_format.clone()));
        self.imp()
            .dropdown_font_family
            .replace(Some(dropdown_font_family.clone()));
        self.imp()
            .dropdown_font_size
            .replace(Some(dropdown_font_size.clone()));
        self.imp().btn_bold.replace(Some(btn_bold.clone()));
        self.imp().btn_italic.replace(Some(btn_italic.clone()));
        self.imp()
            .btn_underline
            .replace(Some(btn_underline.clone()));
        self.imp()
            .btn_strikethrough
            .replace(Some(btn_strikethrough.clone()));
        self.imp().btn_color.replace(Some(btn_color));
        self.imp().btn_list.replace(Some(btn_list.clone()));
        self.imp()
            .btn_ordered_list
            .replace(Some(btn_ordered_list.clone()));
        self.imp()
            .btn_blockquote
            .replace(Some(btn_blockquote.clone()));
        self.imp().btn_align.replace(Some(btn_align));
        self.imp().btn_link.replace(Some(btn_link));
        self.imp().link_popover.replace(Some(link_popover.clone()));
        self.imp().btn_table.replace(Some(btn_table));
        self.imp().table_popover.replace(Some(table_popover));
        self.imp().btn_undo.replace(Some(btn_undo.clone()));
        self.imp().btn_redo.replace(Some(btn_redo.clone()));
        self.imp().format_toolbar.replace(Some(toolbar.clone()));

        let toolbar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .propagate_natural_height(true)
            .min_content_width(0)
            .hexpand(true)
            .css_classes(["format-toolbar-scrolled"])
            .child(&toolbar)
            .build();

        self.imp().toolbar_scroll.replace(Some(toolbar_scroll.clone()));

        let mode_switcher = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .css_classes(["linked", "composer-mode-switcher"])
            .valign(gtk::Align::Center)
            .margin_start(8)
            .margin_end(16)
            .margin_top(6)
            .margin_bottom(6)
            .visible(false)
            .build();

        let btn_mode_html = gtk::ToggleButton::builder()
            .label("HTML")
            .tooltip_text(gettext("Rich Text (HTML)"))
            .active(true)
            .css_classes(["caption-heading"])
            .build();
        set_accessible_label(&btn_mode_html, "HTML");

        let btn_mode_txt = gtk::ToggleButton::builder()
            .label("TXT")
            .tooltip_text(gettext("Plain Text"))
            .group(&btn_mode_html)
            .active(false)
            .css_classes(["caption-heading"])
            .build();
        set_accessible_label(&btn_mode_txt, "TXT");

        mode_switcher.append(&btn_mode_html);
        mode_switcher.append(&btn_mode_txt);

        self.imp().mode_switcher.replace(Some(mode_switcher.clone()));
        self.imp().mode_html_btn.replace(Some(btn_mode_html.clone()));
        self.imp().mode_txt_btn.replace(Some(btn_mode_txt.clone()));

        let editor_weak_html = self.downgrade();
        btn_mode_html.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(editor) = editor_weak_html.upgrade() {
                    editor.set_plain_text_mode(false);
                }
            }
        });
        let mode_switcher_weak = mode_switcher.downgrade();
        btn_mode_html.connect_notify_local(Some("visible"), move |btn, _| {
            if let Some(s) = mode_switcher_weak.upgrade() {
                s.set_visible(btn.is_visible());
            }
        });
        let editor_weak_txt = self.downgrade();
        btn_mode_txt.connect_toggled(move |btn| {
            if btn.is_active() {
                if let Some(editor) = editor_weak_txt.upgrade() {
                    editor.set_plain_text_mode(true);
                }
            }
        });

        let toolbar_container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .build();
        toolbar_container.append(&toolbar_scroll);

        let toolbar_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(180)
            .reveal_child(false)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .child(&toolbar_container)
            .build();

        self.imp()
            .format_toolbar_revealer
            .replace(Some(toolbar_revealer.clone()));

        let top_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .build();
        top_bar.append(&toolbar_revealer);
        top_bar.append(&mode_switcher);

        self.imp().top_bar.replace(Some(top_bar.clone()));

        vbox.append(&top_bar);

        // WebKit6 WebView Canvas
        let web_context = webkit6::WebContext::new();
        let inline_spelling = AppSettings::try_new()
            .map(|settings| settings.composer_inline_spelling())
            .unwrap_or(true);
        web_context.set_spell_checking_enabled(inline_spelling);
        self.imp().spell_checking_enabled.set(inline_spelling);

        let initial_languages = AppSettings::try_new()
            .map(|settings| {
                settings
                    .spellcheck_languages()
                    .split(',')
                    .map(str::trim)
                    .filter(|language| !language.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let initial_lang_refs: Vec<&str> = initial_languages.iter().map(String::as_str).collect();
        web_context.set_spell_checking_languages(&initial_lang_refs);
        self.imp().spell_checking_languages.replace(initial_languages);

        self.imp().web_context.replace(Some(web_context.clone()));

        if let Some(settings) = AppSettings::try_new() {
            let editor_weak = self.downgrade();
            let id_spelling = settings.connect_changed(Some("composer-inline-spelling"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.set_spell_checking_enabled(s.boolean("composer-inline-spelling"));
                }
            });
            let editor_weak = self.downgrade();
            let id_languages = settings.connect_changed(Some("spellcheck-languages"), move |s, _| {
                if let Some(editor) = editor_weak.upgrade() {
                    let raw = s.string("spellcheck-languages");
                    let langs: Vec<&str> = raw
                        .split(',')
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect();
                    editor.set_spell_checking_languages(&langs);
                }
            });
            self.imp().spelling_handler_ids.borrow_mut().push(id_spelling);
            self.imp().spelling_handler_ids.borrow_mut().push(id_languages);
        }

        let user_content_manager = webkit6::UserContentManager::new();
        user_content_manager.register_script_message_handler("meridianDrag", None);
        user_content_manager.register_script_message_handler("meridianImageMenu", None);
        user_content_manager.register_script_message_handler("meridianContentChanged", None);
        user_content_manager.register_script_message_handler("meridianSelectionState", None);

        let editor_weak_for_content_changed = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianContentChanged",
            false,
            move |values| {
                if let Some(editor) = editor_weak_for_content_changed.upgrade() {
                    if let Some(js_val) = values
                        .get(1)
                        .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                    {
                        let raw = js_val.to_str();
                        editor.handle_content_changed_script_message(&raw);
                    }
                }
                None
            },
        );

        let editor_weak_for_drag = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianDrag",
            false,
            move |values| {
                if let Some(editor) = editor_weak_for_drag.upgrade() {
                    if let Some(js_val) = values
                        .get(1)
                        .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                    {
                        let msg = js_val.to_str();
                        editor.handle_drag_script_message(&msg);
                    }
                }
                None
            },
        );

        let editor_weak_for_img_menu = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianImageMenu",
            false,
            move |values| {
                if let Some(editor) = editor_weak_for_img_menu.upgrade() {
                    if let Some(js_val) = values
                        .get(1)
                        .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                    {
                        let msg = js_val.to_str();
                        editor.handle_image_menu_script_message(&msg);
                    }
                }
                None
            },
        );

        let web_view = webkit6::WebView::builder()
            .web_context(&web_context)
            .user_content_manager(&user_content_manager)
            .vexpand(true)
            .hexpand(true)
            .build();
        web_view.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        set_accessible_label(&web_view, &gettext("Email message body"));

        web_view.connect_context_menu(|_, _, hit_test_result| {
            if hit_test_result.context_is_image() {
                return true;
            }
            false
        });

        let font_family_raw = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_family)
            .unwrap_or_else(|| "system-ui, sans-serif".to_string());
        let font_family = crate::views::mail_viewer::safe_css_font_family(&font_family_raw);
        let font_size = AppSettings::try_new()
            .as_ref()
            .map(AppSettings::mail_font_size)
            .unwrap_or(14);

        if let Some(settings) = webkit6::prelude::WebViewExt::settings(&web_view) {
            let desktop_font = crate::views::mail_viewer::detect_desktop_font_family();
            let desktop_mono = crate::views::mail_viewer::detect_desktop_monospace_font_family();
            let primary_font = primary_webkit_font_family(&font_family_raw, &desktop_font);
            settings.set_default_font_family(&primary_font);
            settings.set_sans_serif_font_family(&primary_font);
            settings.set_monospace_font_family(&desktop_mono);
            settings.set_default_font_size(font_size.clamp(8, 48));
            settings.set_enable_javascript(true);
            settings.set_auto_load_images(true);
            settings.set_enable_developer_extras(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_enable_webgl(false);
            settings.set_enable_webrtc(false);
            settings.set_enable_mediasource(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_mock_capture_devices(false);
            settings.set_enable_html5_local_storage(false);
            settings.set_enable_html5_database(false);
            settings.set_enable_offline_web_application_cache(false);
        }
        let is_dark_initial = adw::StyleManager::default().is_dark()
            || AppSettings::try_new()
                .map(|s| s.force_dark_mode())
                .unwrap_or(false);
        let html_theme_class = if is_dark_initial {
            "meridian-dark"
        } else {
            "meridian-light"
        };
        let initial_custom_styles_map = self.imp().custom_styles.borrow().clone();
        let initial_custom_css = build_custom_styles_css(&initial_custom_styles_map);
        let initial_custom_inline_map: std::collections::BTreeMap<String, String> =
            initial_custom_styles_map
                .iter()
                .filter_map(|(tag, st)| {
                    let inline = st.to_inline_css();
                    if inline.is_empty() {
                        None
                    } else {
                        Some((tag.clone(), inline))
                    }
                })
                .collect();
        let initial_custom_map_json =
            serde_json::to_string(&initial_custom_inline_map).unwrap_or_else(|_| "{}".to_string());
        let initial_html = format!(
            concat!(
                "<!DOCTYPE html><html class=\"{}\"><head><style>",
                "html, body {{ font-family: {}; font-size: {}px; line-height: 1.35; color: #222; margin: 12px; background-color: transparent; }}",
                "p {{ margin: 0; }}",
                "p.MsoNormal, li.MsoNormal, div.MsoNormal, blockquote p {{ margin: 0; line-height: 1.35; min-height: 1.35em; }}",
                "img {{ max-width: 100%; height: auto; cursor: grab; -webkit-user-drag: element; }}",
                "img:active {{ cursor: grabbing; }}",
                "a {{ color: #1a73e8; text-decoration: underline; cursor: pointer; }}",
                "blockquote, blockquote[type=\"cite\"] {{ margin: 4px 0 4px 4px; border-left: 3px solid #3584e4; padding: 2px 0 2px 12px; }}",
                "blockquote + blockquote {{ margin-top: 0; padding-top: 2px; }}",
                "blockquote > :first-child {{ margin-top: 0; }}",
                "code {{ font-family: monospace; background-color: rgba(128, 128, 128, 0.14); padding: 1px 4px; border-radius: 4px; font-size: 0.92em; }}",
                "pre {{ font-family: monospace; background-color: rgba(128, 128, 128, 0.10); padding: 8px 10px; border-radius: 6px; overflow-x: auto; margin: 4px 0; }}",
                "table {{ border-collapse: collapse; width: calc(100% - 28px); margin: 6px 0 28px 0; }}",
                "td, th {{ border: 1px solid rgba(128, 128, 128, 0.38); padding: 6px 8px; min-width: 40px; vertical-align: top; }}",
                "html.meridian-dark {{ color-scheme: dark; background-color: #e1e1e1 !important; color: #111111; filter: invert(100%) hue-rotate(180deg); -webkit-filter: invert(100%) hue-rotate(180deg); }}",
                "html.meridian-dark body {{ background-color: transparent !important; color: #111111; }}",
                "html.meridian-dark img, html.meridian-dark video, html.meridian-dark canvas, html.meridian-dark svg {{ filter: invert(100%) hue-rotate(180deg) !important; -webkit-filter: invert(100%) hue-rotate(180deg) !important; }}",
                "@media (prefers-color-scheme: dark) {{",
                "  html:not(.meridian-light) {{ color-scheme: dark; background-color: #e1e1e1 !important; color: #111111; filter: invert(100%) hue-rotate(180deg); -webkit-filter: invert(100%) hue-rotate(180deg); }}",
                "  html:not(.meridian-light) body {{ background-color: transparent !important; color: #111111; }}",
                "  html:not(.meridian-light) img, html:not(.meridian-light) video, html:not(.meridian-light) canvas, html:not(.meridian-light) svg {{ filter: invert(100%) hue-rotate(180deg) !important; -webkit-filter: invert(100%) hue-rotate(180deg) !important; }}",
                "}}",
                "</style>",
                "<style id=\"__meridian_custom_styles\">{}</style>",
                "<script>",
                "window.__meridianCustomStyles = {};",
                "window.meridianSetCustomStyles = function(stylesMap, cssText) {{",
                "    window.__meridianCustomStyles = stylesMap || {{}};",
                "    let stEl = document.getElementById('__meridian_custom_styles');",
                "    if (!stEl) {{",
                "        stEl = document.createElement('style');",
                "        stEl.id = '__meridian_custom_styles';",
                "        document.head.appendChild(stEl);",
                "    }}",
                "    stEl.textContent = cssText || '';",
                "    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "}};",
                "document.addEventListener('mouseover', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && !link.title) {{",
                "        link.title = link.href + ' (Ctrl+Click to open)';",
                "    }}",
                "}});",
                "document.addEventListener('click', function(e) {{",
                "    const link = e.target.closest('a');",
                "    if (link && link.href && (e.ctrlKey || e.metaKey)) {{",
                "        e.preventDefault();",
                "        window.open(link.href, '_blank');",
                "    }}",
                "}});",
                "document.addEventListener('paste', function(e) {{",
                "    if (!e.clipboardData) return;",
                "    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "    const items = e.clipboardData.items;",
                "    if (items) {{",
                "        for (let i = 0; i < items.length; i++) {{",
                "            if (items[i].type.indexOf('image') !== -1) {{",
                "                const file = items[i].getAsFile();",
                "                if (file) {{",
                "                    e.preventDefault();",
                "                    const reader = new FileReader();",
                "                    reader.onload = function(evt) {{",
                "                        const img = new Image();",
                "                        img.onload = function() {{",
                "                            if (img.width > 1920 || img.height > 1920) {{",
                "                                const scale = 1920 / Math.max(img.width, img.height);",
                "                                const cv = document.createElement('canvas');",
                "                                cv.width = Math.round(img.width * scale);",
                "                                cv.height = Math.round(img.height * scale);",
                "                                const ctx = cv.getContext('2d');",
                "                                ctx.drawImage(img, 0, 0, cv.width, cv.height);",
                "                                document.execCommand('insertImage', false, cv.toDataURL('image/jpeg', 0.85));",
                "                            }} else {{",
                "                                document.execCommand('insertImage', false, evt.target.result);",
                "                            }}",
                "                            if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "                            if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                        }};",
                "                        img.src = evt.target.result;",
                "                    }};",
                "                    reader.readAsDataURL(file);",
                "                    return;",
                "                }}",
                "            }}",
                "        }}",
                "    }}",
                "    const types = Array.from(e.clipboardData.types || []);",
                "    if (types.includes('text/html')) {{",
                "        const rawHtml = e.clipboardData.getData('text/html');",
                "        if (rawHtml) {{",
                "            e.preventDefault();",
                "            let cleanHtml = rawHtml;",
                "            try {{",
                "                const parser = new DOMParser();",
                "                const doc = parser.parseFromString(rawHtml, 'text/html');",
                "                const isWhite = function(v) {{",
                "                    if (!v) return false;",
                "                    const s = v.toLowerCase().replace(/\\s+/g, '');",
                "                    return s === 'rgb(255,255,255)' || s === 'rgba(255,255,255,1)' || s === '#ffffff' || s === '#fff' || s === 'white' || s === 'transparent' || s === 'rgba(0,0,0,0)';",
                "                }};",
                "                const all = doc.querySelectorAll('*');",
                "                for (let j = 0; j < all.length; j++) {{",
                "                    const el = all[j];",
                "                    if (isWhite(el.style.backgroundColor)) {{ el.style.removeProperty('background-color'); }}",
                "                    if (isWhite(el.style.background)) {{ el.style.removeProperty('background'); }}",
                "                    if (el.style.caretColor) {{ el.style.removeProperty('caret-color'); }}",
                "                    if (el.hasAttribute('bgcolor') && isWhite(el.getAttribute('bgcolor'))) {{ el.removeAttribute('bgcolor'); }}",
                "                    if (el.getAttribute('style') !== null && el.style.length === 0) {{ el.removeAttribute('style'); }}",
                "                }}",
                "                cleanHtml = doc.body.innerHTML;",
                "            }} catch (err) {{",
                "                cleanHtml = rawHtml",
                "                    .replace(/(?:background-color|background)\\s*:\\s*(?:rgb\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*\\)|rgba\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*,\\s*1(?:\\.0+)?\\s*\\)|#(?:ffffff|fff)\\b|white\\b)\\s*(?:!\\s*important)?\\s*;?/gi, '')",
                "                    .replace(/caret-color\\s*:\\s*[^;\"]+;?/gi, '')",
                "                    .replace(/\\s*bgcolor\\s*=\\s*[\"']\\s*(?:#ffffff|#fff|white|rgb\\(\\s*255\\s*,\\s*255\\s*,\\s*255\\s*\\))\\s*[\"']/gi, '');",
                "            }}",
                "            if (cleanHtml && cleanHtml.trim().length > 0) {{",
                "                document.execCommand('insertHTML', false, cleanHtml);",
                "                if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "                if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                return;",
                "            }}",
                "        }}",
                "    }}",
                "    const text = e.clipboardData.getData('text/plain');",
                "    if (text) {{",
                "        const urlRegex = /(https?:\\/\\/[^\\s<]+|mailto:[^\\s<]+|www\\.[a-zA-Z0-9.\\-]+\\.[a-zA-Z]{{2,}}[^\\s<]*|[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{{2,}})/g;",
                "        if (urlRegex.test(text)) {{",
                "            e.preventDefault();",
                "            const escaped = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');",
                "            const htmlWithLinks = escaped.replace(urlRegex, function(match) {{",
                "                let cleanMatch = match;",
                "                let trailing = '';",
                "                while (/[.,;:)]$/.test(cleanMatch)) {{",
                "                    trailing = cleanMatch.slice(-1) + trailing;",
                "                    cleanMatch = cleanMatch.slice(0, -1);",
                "                }}",
                "                let href = cleanMatch;",
                "                if (cleanMatch.startsWith('www.')) {{",
                "                    href = 'https://' + cleanMatch;",
                "                }} else if (cleanMatch.includes('@') && !cleanMatch.startsWith('mailto:') && !cleanMatch.startsWith('http')) {{",
                "                    href = 'mailto:' + cleanMatch;",
                "                }}",
                "                return '<a href=\"' + href + '\">' + cleanMatch + '</a>' + trailing;",
                "            }}).replace(/\\r?\\n/g, '<br>');",
                "            document.execCommand('insertHTML', false, htmlWithLinks);",
                "            if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "            if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        }}",
                "    }}",
                "}});",
                "window.__plainTextMode = false;",
                "document.addEventListener('contextmenu', function(e) {{",
                "    try {{",
                "        const target = e.target;",
                "        let img = null;",
                "        if (target) {{",
                "            if (target.nodeName === 'IMG') {{",
                "                img = target;",
                "            }} else if (target.closest) {{",
                "                img = target.closest('img');",
                "            }}",
                "            if (!img && target.querySelector) {{",
                "                img = target.querySelector('img');",
                "            }}",
                "        }}",
                "        if (img && !window.__plainTextMode) {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            if (!img.id) {{",
                "                img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);",
                "            }}",
                "            if (!img.getAttribute('data-meridian-id')) {{",
                "                img.setAttribute('data-meridian-id', img.id);",
                "            }}",
                "        const rect = img.getBoundingClientRect();",
                "        let align = 'inline';",
                "        if (img.style.display === 'block') {{",
                "            if (img.style.marginLeft === 'auto' && img.style.marginRight === 'auto') {{",
                "                align = 'center';",
                "            }} else if (img.style.marginLeft === 'auto' || img.getAttribute('align') === 'right') {{",
                "                align = 'right';",
                "            }} else {{",
                "                align = 'left';",
                "            }}",
                "        }} else if (img.style.float === 'left') {{",
                "            align = 'wrap_left';",
                "        }} else if (img.style.float === 'right') {{",
                "            align = 'wrap_right';",
                "        }} else if (img.getAttribute('align') === 'left') {{",
                "            align = 'left';",
                "        }} else if (img.getAttribute('align') === 'right') {{",
                "            align = 'right';",
                "        }} else if (img.getAttribute('align') === 'center') {{",
                "            align = 'center';",
                "        }}",
                "        let size = 'original';",
                "        const w = img.style.width || img.style.maxWidth || '';",
                "        if (w === '25%') size = 'small';",
                "        else if (w === '50%') size = 'medium';",
                "        else if (w === '100%') size = 'large';",
                "        const payload = JSON.stringify({{",
                "            id: img.id,",
                "            x: Math.max(0, Math.round(e.clientX)),",
                "            y: Math.max(0, Math.round(e.clientY)),",
                "            img_x: Math.round(rect.left),",
                "            img_y: Math.round(rect.top),",
                "            img_w: Math.round(rect.width),",
                "            img_h: Math.round(rect.height),",
                "            align: align,",
                "            size: size",
                "        }});",
                "        try {{",
                "            window.webkit.messageHandlers.meridianImageMenu.postMessage(payload);",
                "        }} catch(err) {{}}",
                "        return false;",
                "    }}",
                "    }} catch(err) {{}}",
                "}});",
                "let lastDragNotify = 0;",
                "let isInternalDrag = false;",
                "let draggedElement = null;",
                "let currentDropRange = null;",
                "let currentDropInfo = null;",
                "let lastValidDropRange = null;",
                "let lastValidDropInfo = null;",
                "let dropIndicator = null;",
                "function resetDraggedElement() {{",
                "    if (draggedElement) {{",
                "        try {{",
                "            draggedElement.style.opacity = '';",
                "            draggedElement.style.pointerEvents = '';",
                "            if (draggedElement.parentElement && draggedElement.parentElement !== document.body) {{",
                "                draggedElement.parentElement.style.pointerEvents = '';",
                "            }}",
                "        }} catch(e) {{}}",
                "    }}",
                "}};",
                "document.addEventListener('dragstart', function(e) {{",
                "    const img = (e.target && e.target.nodeName === 'IMG') ? e.target : (e.target.querySelector ? e.target.querySelector('img') : (e.target.closest ? e.target.closest('img') : null));",
                "    if (img) {{",
                "        isInternalDrag = true;",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_start'); }} catch(err) {{}}",
                "        draggedElement = img;",
                "        if (!img.id) {{",
                "            img.id = '__meridian_img_' + Date.now() + '_' + Math.floor(Math.random() * 10000);",
                "        }}",
                "        if (!img.getAttribute('data-meridian-id')) {{",
                "            img.setAttribute('data-meridian-id', img.id);",
                "        }}",
                "        if (e.dataTransfer) {{",
                "            e.dataTransfer.effectAllowed = 'move';",
                "            try {{",
                "                e.dataTransfer.setData('text/plain', '');",
                "            }} catch(err) {{}}",
                "            try {{",
                "                const rect = img.getBoundingClientRect();",
                "                const clickX = Math.max(0, Math.min(Math.round(rect.width), Math.round(e.clientX - rect.left)));",
                "                const clickY = Math.max(0, Math.min(Math.round(rect.height), Math.round(e.clientY - rect.top)));",
                "                if (e.dataTransfer.setDragImage) {{",
                "                    e.dataTransfer.setDragImage(img, clickX, clickY);",
                "                }}",
                "            }} catch(err) {{}}",
                "        }}",
                "        setTimeout(() => {{",
                "            if (draggedElement === img) {{",
                "                img.style.opacity = '0.35';",
                "                img.style.pointerEvents = 'none';",
                "                if (img.parentElement && img.parentElement !== document.body) {{",
                "                    img.parentElement.style.pointerEvents = 'none';",
                "                }}",
                "            }}",
                "        }}, 0);",
                "    }} else {{",
                "        isInternalDrag = false;",
                "        draggedElement = null;",
                "    }}",
                "}});",
                "function handleDragEnd() {{",
                "    resetDraggedElement();",
                "    if (isInternalDrag) {{",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "    }}",
                "    isInternalDrag = false;",
                "    draggedElement = null;",
                "    currentDropInfo = null;",
                "    currentDropRange = null;",
                "    lastValidDropInfo = null;",
                "    lastValidDropRange = null;",
                "    dragCounter = 0;",
                "    hideCaret();",
                "}};",
                "document.addEventListener('dragend', handleDragEnd);",
                "window.addEventListener('dragend', handleDragEnd);",
                "function checkHasFiles(e) {{",
                "    if (isInternalDrag) return false;",
                "    if (!e.dataTransfer || !e.dataTransfer.types) return false;",
                "    for (let i = 0; i < e.dataTransfer.types.length; i++) {{",
                "        const t = e.dataTransfer.types[i];",
                "        if (t === 'Files' || t === 'application/x-moz-file' || t === 'text/uri-list') {{",
                "            return true;",
                "        }}",
                "    }}",
                "    return false;",
                "}};",
                "function getDropIndicator() {{",
                "    if (!dropIndicator || !dropIndicator.parentNode) {{",
                "        dropIndicator = document.createElement('div');",
                "        dropIndicator.id = '__meridian_drop_caret';",
                "        dropIndicator.style.position = 'fixed';",
                "        dropIndicator.style.width = '2px';",
                "        dropIndicator.style.backgroundColor = '#3584e4';",
                "        dropIndicator.style.borderRadius = '1px';",
                "        dropIndicator.style.pointerEvents = 'none';",
                "        dropIndicator.style.zIndex = '2147483647';",
                "        dropIndicator.style.boxShadow = '0 0 3px rgba(53, 132, 228, 0.8)';",
                "        dropIndicator.style.animation = 'none';",
                "        dropIndicator.style.display = 'none';",
                "        document.body.appendChild(dropIndicator);",
                "    }}",
                "    return dropIndicator;",
                "}};",
                "function hideCaret() {{",
                "    const el = document.getElementById('__meridian_drop_caret');",
                "    if (el) el.style.display = 'none';",
                "}};",
                "function getCaretRect(range, y) {{",
                "    if (!range) return null;",
                "    let rect = range.getBoundingClientRect();",
                "    if (rect && rect.height > 0 && (rect.top > 0 || rect.bottom > 0)) {{",
                "        return rect;",
                "    }}",
                "    const rects = range.getClientRects();",
                "    if (rects && rects.length > 0 && rects[0].height > 0) {{",
                "        return rects[0];",
                "    }}",
                "    try {{",
                "        const container = range.startContainer;",
                "        const offset = range.startOffset;",
                "        if (container.nodeType === 3) {{",
                "            const len = container.nodeValue ? container.nodeValue.length : 0;",
                "            if (len > 0) {{",
                "                if (offset < len) {{",
                "                    const tr = document.createRange();",
                "                    tr.setStart(container, offset);",
                "                    tr.setEnd(container, offset + 1);",
                "                    const cr = tr.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        return {{ left: cr.left, right: cr.left, top: cr.top, bottom: cr.bottom, width: 0, height: cr.height }};",
                "                    }}",
                "                }}",
                "                if (offset > 0) {{",
                "                    const tr = document.createRange();",
                "                    tr.setStart(container, offset - 1);",
                "                    tr.setEnd(container, offset);",
                "                    const cr = tr.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        return {{ left: cr.right, right: cr.right, top: cr.top, bottom: cr.bottom, width: 0, height: cr.height }};",
                "                    }}",
                "                }}",
                "            }}",
                "        }} else if (container.nodeType === 1) {{",
                "            if (offset < container.childNodes.length) {{",
                "                const child = container.childNodes[offset];",
                "                if (child && child.getBoundingClientRect) {{",
                "                    const cr = child.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        const h = 20;",
                "                        const t = (cr.height > 24 && y !== undefined) ? Math.max(cr.top, Math.min(cr.bottom - h, y - h / 2)) : cr.top;",
                "                        return {{ left: cr.left, right: cr.left, top: t, bottom: t + h, width: 0, height: h }};",
                "                    }}",
                "                }}",
                "            }}",
                "            if (offset > 0) {{",
                "                const child = container.childNodes[offset - 1];",
                "                if (child && child.getBoundingClientRect) {{",
                "                    const cr = child.getBoundingClientRect();",
                "                    if (cr && cr.height > 0) {{",
                "                        const h = 20;",
                "                        const t = (cr.height > 24 && y !== undefined) ? Math.max(cr.top, Math.min(cr.bottom - h, y - h / 2)) : cr.top;",
                "                        return {{ left: cr.right, right: cr.right, top: t, bottom: t + h, width: 0, height: h }};",
                "                    }}",
                "                }}",
                "            }}",
                "            const pr = container.getBoundingClientRect();",
                "            if (pr && pr.height > 0) {{",
                "                const h = 20;",
                "                const t = (pr.height > 24 && y !== undefined) ? Math.max(pr.top, Math.min(pr.bottom - h, y - h / 2)) : pr.top;",
                "                return {{ left: pr.left, right: pr.left, top: t, bottom: t + h, width: 0, height: h }};",
                "            }}",
                "        }}",
                "    }} catch(e) {{}}",
                "    return null;",
                "}};",
                "function getSignatureElement() {{",
                "    return document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');",
                "}};",
                "function isBlockEmpty(el) {{",
                "    if (!el) return true;",
                "    if (el.nodeType === 3) {{",
                "        return el.textContent.replace(/[\\s\\u200b\\u00a0]+/g, '') === '';",
                "    }}",
                "    const text = el.textContent ? el.textContent.replace(/[\\s\\u200b\\u00a0]+/g, '') : '';",
                "    if (text === '') {{",
                "        const imgs = el.querySelectorAll ? el.querySelectorAll('img') : [];",
                "        return imgs.length === 0;",
                "    }}",
                "    return false;",
                "}};",
                "function getContentBlocks(excludeEl) {{",
                "    const sig = getSignatureElement();",
                "    const blocks = [];",
                "    for (let i = 0; i < document.body.children.length; i++) {{",
                "        const child = document.body.children[i];",
                "        if (child === sig || child.id === '__meridian_drop_caret' || child.tagName === 'STYLE' || child.tagName === 'SCRIPT') continue;",
                "        if (excludeEl && (child === excludeEl || child.contains(excludeEl))) continue;",
                "        blocks.push(child);",
                "    }}",
                "    return blocks;",
                "}};",
                "function constrainRangeBeforeSignature(range) {{",
                "    if (!range) return null;",
                "    const sig = getSignatureElement();",
                "    if (!sig) return range;",
                "    let node = range.startContainer;",
                "    let isAfterOrInside = (node === sig || sig.contains(node));",
                "    if (!isAfterOrInside) {{",
                "        try {{",
                "            const cmp = sig.compareDocumentPosition(node);",
                "            if (cmp & Node.DOCUMENT_POSITION_FOLLOWING) {{",
                "                isAfterOrInside = true;",
                "            }} else if (node === document.body) {{",
                "                const sigIdx = Array.prototype.indexOf.call(document.body.childNodes, sig);",
                "                if (sigIdx !== -1 && range.startOffset > sigIdx) {{",
                "                    isAfterOrInside = true;",
                "                }}",
                "            }}",
                "        }} catch(e) {{}}",
                "    }}",
                "    if (isAfterOrInside) {{",
                "        let prev = sig.previousElementSibling;",
                "        if (!prev) {{",
                "            prev = document.createElement('p');",
                "            prev.innerHTML = '<br>';",
                "            document.body.insertBefore(prev, sig);",
                "        }}",
                "        const safeRange = document.createRange();",
                "        safeRange.selectNodeContents(prev);",
                "        safeRange.collapse(false);",
                "        return safeRange;",
                "    }}",
                "    return range;",
                "}};",
                "function updateCaretToPoint(x, y) {{",
                "    if (window.__plainTextMode) return null;",
                "    try {{",
                "        const blocks = getContentBlocks(draggedElement);",
                "        const firstBlock = blocks.length > 0 ? blocks[0] : null;",
                "        const fbRect = firstBlock ? firstBlock.getBoundingClientRect() : null;",
                "        const isNearTop = fbRect",
                "            ? (isBlockEmpty(firstBlock) ? (y <= Math.max(fbRect.bottom, 40)) : (y < fbRect.top - 2))",
                "            : (y <= 40);",
                "        if (firstBlock && isNearTop) {{",
                "            currentDropInfo = {{ isTop: true, block: firstBlock }};",
                "            currentDropRange = null;",
                "            lastValidDropInfo = currentDropInfo;",
                "            lastValidDropRange = null;",
                "            const el = getDropIndicator();",
                "            const caretH = 20;",
                "            const top = Math.round(fbRect.top + (fbRect.height > 20 ? (fbRect.height - 20) / 2 : 0));",
                "            el.style.left = Math.round(fbRect.left) + 'px';",
                "            el.style.top = top + 'px';",
                "            el.style.width = '2px';",
                "            el.style.height = caretH + 'px';",
                "            el.style.display = 'block';",
                "            return currentDropInfo;",
                "        }}",
                "        currentDropInfo = null;",
                "        let range = null;",
                "        if (document.caretRangeFromPoint) {{",
                "            range = document.caretRangeFromPoint(x, y);",
                "        }} else if (document.caretPositionFromPoint) {{",
                "            const pos = document.caretPositionFromPoint(x, y);",
                "            if (pos) {{",
                "                range = document.createRange();",
                "                range.setStart(pos.offsetNode, pos.offset);",
                "                range.collapse(true);",
                "            }}",
                "        }}",
                "        if (!range && blocks.length > 0) {{",
                "            const lastBlock = blocks[blocks.length - 1];",
                "            const lbRect = lastBlock.getBoundingClientRect();",
                "            if (y >= lbRect.bottom) {{",
                "                const fallbackRange = document.createRange();",
                "                fallbackRange.selectNodeContents(lastBlock);",
                "                fallbackRange.collapse(false);",
                "                range = fallbackRange;",
                "            }}",
                "        }}",
                "        if (range) {{",
                "            range = constrainRangeBeforeSignature(range);",
                "            if (draggedElement && (draggedElement.contains(range.startContainer) || draggedElement === range.startContainer || (draggedElement.parentElement && draggedElement.parentElement === range.startContainer && draggedElement.parentElement.children.length === 1))) {{",
                "                hideCaret();",
                "                return null;",
                "            }}",
                "            const rect = getCaretRect(range, y);",
                "            if (rect && rect.height > 0) {{",
                "                const el = getDropIndicator();",
                "                const caretH = 20;",
                "                const top = Math.round(rect.top + (rect.height > 20 ? (rect.height - 20) / 2 : 0));",
                "                el.style.left = Math.round(rect.left) + 'px';",
                "                el.style.top = top + 'px';",
                "                el.style.width = '2px';",
                "                el.style.height = caretH + 'px';",
                "                el.style.display = 'block';",
                "                currentDropRange = range.cloneRange();",
                "                lastValidDropRange = currentDropRange;",
                "                lastValidDropInfo = null;",
                "                return currentDropRange;",
                "            }}",
                "        }}",
                "    }} catch(err) {{}}",
                "    hideCaret();",
                "    return null;",
                "}};",
                "let dragCounter = 0;",
                "document.addEventListener('dragenter', function(e) {{",
                "    if (isInternalDrag) {{",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        return;",
                "    }}",
                "    if (checkHasFiles(e)) {{",
                "        dragCounter++;",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('enter'); }} catch(err) {{}}",
                "    }}",
                "}});",
                "document.addEventListener('dragover', function(e) {{",
                "    if (isInternalDrag) {{",
                "        e.preventDefault();",
                "        if (e.dataTransfer) {{ e.dataTransfer.dropEffect = 'move'; }}",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        return;",
                "    }}",
                "    if (!checkHasFiles(e)) return;",
                "    e.preventDefault();",
                "    if (e.dataTransfer) {{ e.dataTransfer.dropEffect = 'copy'; }}",
                "    updateCaretToPoint(e.clientX, e.clientY);",
                "    const now = Date.now();",
                "    if (now - lastDragNotify > 40) {{",
                "        lastDragNotify = now;",
                "        try {{",
                "            window.webkit.messageHandlers.meridianDrag.postMessage('motion:' + Math.round(e.clientY) + ':' + Math.round(window.innerHeight));",
                "        }} catch(err) {{}}",
                "    }}",
                "}});",
                "document.addEventListener('dragleave', function(e) {{",
                "    dragCounter--;",
                "    const isOutOfWindow = e.clientY <= 0 || e.clientX <= 0 || e.clientX >= window.innerWidth || e.clientY >= window.innerHeight;",
                "    if (dragCounter <= 0 || isOutOfWindow) {{",
                "        dragCounter = 0;",
                "        hideCaret();",
                "        if (!isInternalDrag) {{",
                "            try {{ window.webkit.messageHandlers.meridianDrag.postMessage('leave'); }} catch(err) {{}}",
                "        }}",
                "    }}",
                "}});",
                "document.addEventListener('drop', function(e) {{",
                "    dragCounter = 0;",
                "    hideCaret();",
                "    let dropInfo = currentDropInfo || lastValidDropInfo;",
                "    currentDropInfo = null;",
                "    let dropRange = currentDropRange || lastValidDropRange;",
                "    currentDropRange = null;",
                "    if (!dropInfo && !dropRange && e.clientX > 0 && e.clientY > 0) {{",
                "        updateCaretToPoint(e.clientX, e.clientY);",
                "        dropInfo = currentDropInfo || lastValidDropInfo;",
                "        currentDropInfo = null;",
                "        dropRange = currentDropRange || lastValidDropRange;",
                "        currentDropRange = null;",
                "    }}",
                "    hideCaret();",
                "    const sig = getSignatureElement();",
                "    if (isInternalDrag && draggedElement) {{",
                "        e.preventDefault();",
                "        try {{",
                "            resetDraggedElement();",
                "            const oldParent = draggedElement.parentElement;",
                "            if (dropInfo && dropInfo.isTop) {{",
                "                const blocks = getContentBlocks(draggedElement);",
                "                const firstBlock = blocks.length > 0 ? blocks[0] : null;",
                "                if (firstBlock && isBlockEmpty(firstBlock) && firstBlock !== sig) {{",
                "                    firstBlock.innerHTML = '';",
                "                    firstBlock.appendChild(draggedElement);",
                "                }} else {{",
                "                    let topElem = draggedElement;",
                "                    if (!topElem || topElem.nodeName !== 'P') {{",
                "                        const newP = document.createElement('p');",
                "                        newP.style.margin = '4px 0';",
                "                        newP.appendChild(draggedElement);",
                "                        topElem = newP;",
                "                    }}",
                "                    if (firstBlock) {{",
                "                        document.body.insertBefore(topElem, firstBlock);",
                "                    }} else if (sig) {{",
                "                        document.body.insertBefore(topElem, sig);",
                "                    }} else {{",
                "                        document.body.appendChild(topElem);",
                "                    }}",
                "                }}",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "                const allBlocks = getContentBlocks();",
                "                for (let i = 0; i < allBlocks.length; i++) {{",
                "                    const b = allBlocks[i];",
                "                    if (b === sig || b.contains(draggedElement)) break;",
                "                    if (isBlockEmpty(b)) {{",
                "                        b.remove();",
                "                    }} else {{",
                "                        break;",
                "                    }}",
                "                }}",
                "            }} else if (dropRange) {{",
                "                dropRange = constrainRangeBeforeSignature(dropRange);",
                "                let container = dropRange.startContainer;",
                "                let block = (container.nodeType === 1) ? container : container.parentElement;",
                "                block = block ? block.closest('p, div, li, blockquote') : null;",
                "                if (container === document.body) {{",
                "                    const childAtOffset = document.body.childNodes[dropRange.startOffset];",
                "                    const prevChild = dropRange.startOffset > 0 ? document.body.childNodes[dropRange.startOffset - 1] : null;",
                "                    if (prevChild && isBlockEmpty(prevChild) && prevChild !== sig) {{",
                "                        block = prevChild;",
                "                    }} else if (childAtOffset && isBlockEmpty(childAtOffset) && childAtOffset !== sig) {{",
                "                        block = childAtOffset;",
                "                    }}",
                "                }}",
                "                if (block && isBlockEmpty(block) && block !== sig) {{",
                "                    block.innerHTML = '';",
                "                    block.appendChild(draggedElement);",
                "                }} else if (container.nodeType === 1 && container.childNodes.length === 1 && container.childNodes[0].nodeName === 'BR') {{",
                "                    container.innerHTML = '';",
                "                    container.appendChild(draggedElement);",
                "                }} else {{",
                "                    let toInsert = draggedElement;",
                "                    if (container === document.body && toInsert.nodeName === 'IMG') {{",
                "                        const newP = document.createElement('p');",
                "                        newP.style.margin = '4px 0';",
                "                        newP.appendChild(toInsert);",
                "                        toInsert = newP;",
                "                    }}",
                "                    dropRange.insertNode(toInsert);",
                "                }}",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }} else if (sig) {{",
                "                let fallbackElem = draggedElement;",
                "                if (!fallbackElem || fallbackElem.nodeName !== 'P') {{",
                "                    const newP = document.createElement('p');",
                "                    newP.style.margin = '4px 0';",
                "                    newP.appendChild(draggedElement);",
                "                    fallbackElem = newP;",
                "                }}",
                "                document.body.insertBefore(fallbackElem, sig);",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }} else {{",
                "                let fallbackElem = draggedElement;",
                "                if (!fallbackElem || fallbackElem.nodeName !== 'P') {{",
                "                    const newP = document.createElement('p');",
                "                    newP.style.margin = '4px 0';",
                "                    newP.appendChild(draggedElement);",
                "                    fallbackElem = newP;",
                "                }}",
                "                document.body.appendChild(fallbackElem);",
                "                if (oldParent && oldParent !== document.body && oldParent !== draggedElement.parentElement && isBlockEmpty(oldParent)) {{",
                "                    oldParent.remove();",
                "                }}",
                "            }}",
                "            const sel = window.getSelection();",
                "            if (sel) {{",
                "                const afterRange = document.createRange();",
                "                afterRange.setStartAfter(draggedElement);",
                "                afterRange.collapse(true);",
                "                sel.removeAllRanges();",
                "                sel.addRange(afterRange);",
                "            }}",
                "        }} catch(err) {{",
                "            console.error('Drop error:', err);",
                "        }} finally {{",
                "            resetDraggedElement();",
                "            if (isInternalDrag) {{",
                "                try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "            }}",
                "            isInternalDrag = false;",
                "            draggedElement = null;",
                "            currentDropInfo = null;",
                "            currentDropRange = null;",
                "            lastValidDropInfo = null;",
                "            lastValidDropRange = null;",
                "            hideCaret();",
                "        }}",
                "        return;",
                "    }}",
                "    if (isInternalDrag) {{",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('internal_end'); }} catch(err) {{}}",
                "    }}",
                "    resetDraggedElement();",
                "    isInternalDrag = false;",
                "    draggedElement = null;",
                "    try {{ window.webkit.messageHandlers.meridianDrag.postMessage('drop'); }} catch(err) {{}}",
                "    e.preventDefault();",
                "    if (dropInfo && dropInfo.isTop) {{",
                "        window.__lastDropWasTop = true;",
                "    }} else {{",
                "        window.__lastDropWasTop = false;",
                "    }}",
                "    if (dropRange) {{",
                "        dropRange = constrainRangeBeforeSignature(dropRange);",
                "        try {{",
                "            const sel = window.getSelection();",
                "            sel.removeAllRanges();",
                "            sel.addRange(dropRange);",
                "        }} catch(err) {{}}",
                "    }}",
                "}});",
                "function handleEscape(e) {{",
                "    if (e.key === 'Escape') {{",
                "        handleDragEnd();",
                "        try {{ window.webkit.messageHandlers.meridianDrag.postMessage('cancel'); }} catch(err) {{}}",
                "    }}",
                "}};",
                "window.addEventListener('keydown', handleEscape);",
                "document.addEventListener('keydown', handleEscape);",
                "window.addEventListener('mouseleave', function(e) {{ hideCaret(); }});",
                "document.addEventListener('mouseleave', function(e) {{ hideCaret(); }});",
                "window.addEventListener('scroll', function() {{ hideCaret(); }}, true);",
                "function meridianExtractPlainText(root) {{",
                "    const blocks = new Set(['P', 'DIV', 'LI', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'PRE', 'BLOCKQUOTE', 'TR']);",
                "    let out = '';",
                "    const walk = function(node) {{",
                "        if (!node) return;",
                "        if (node.nodeType === 3) {{",
                "            out += node.nodeValue;",
                "            return;",
                "        }}",
                "        if (node.nodeType !== 1) return;",
                "        const tag = node.tagName;",
                "        if (tag === 'STYLE' || tag === 'SCRIPT') return;",
                "        if (tag === 'BR') {{",
                "            out += '\\n';",
                "            return;",
                "        }}",
                "        const isBlock = blocks.has(tag);",
                "        if (isBlock && out.length > 0 && !out.endsWith('\\n')) {{",
                "            out += '\\n';",
                "        }}",
                "        for (let i = 0; i < node.childNodes.length; i++) {{",
                "            walk(node.childNodes[i]);",
                "        }}",
                "        if (isBlock && !out.endsWith('\\n')) {{",
                "            out += '\\n';",
                "        }}",
                "    }};",
                "    walk(root);",
                "    return out.replace(/\\u00a0/g, ' ').replace(/\\n+$/, '');",
                "}};",
                "window.meridianExtractPlainText = meridianExtractPlainText;",
                "window.meridianUnwrapBlockquote = function() {{",
                "    const sel = window.getSelection();",
                "    if (!sel || sel.rangeCount === 0) return false;",
                "    const range = sel.getRangeAt(0);",
                "    let startNode = range.startContainer;",
                "    if (startNode && startNode.nodeType === 3) startNode = startNode.parentElement;",
                "    let endNode = range.endContainer;",
                "    if (endNode && endNode.nodeType === 3) endNode = endNode.parentElement;",
                "    const targetQuotes = Array.from(document.querySelectorAll('blockquote')).filter(function(q) {{",
                "        return sel.containsNode(q, true) || (startNode && q.contains(startNode)) || (endNode && q.contains(endNode));",
                "    }});",
                "    if (targetQuotes.length === 0) return false;",
                "    targetQuotes.sort(function(a, b) {{ return a.contains(b) ? 1 : -1; }});",
                "    const targetBq = targetQuotes[0];",
                "    const children = Array.from(targetBq.childNodes);",
                "    const selectedChildren = sel.isCollapsed",
                "        ? children.filter(function(c) {{ return (startNode && c.contains(startNode)) || c === startNode; }})",
                "        : children.filter(function(c) {{ return sel.containsNode(c, true) || (startNode && c.contains(startNode)) || (endNode && c.contains(endNode)); }});",
                "    const parent = targetBq.parentNode;",
                "    if (!parent) return false;",
                "    if (selectedChildren.length === 0 || selectedChildren.length === children.length) {{",
                "        while (targetBq.firstChild) {{",
                "            const child = targetBq.firstChild;",
                "            if (child.nodeType === 3 && child.textContent.trim().length > 0) {{",
                "                const p = document.createElement('p');",
                "                p.appendChild(child);",
                "                parent.insertBefore(p, targetBq);",
                "            }} else {{",
                "                parent.insertBefore(child, targetBq);",
                "            }}",
                "        }}",
                "        parent.removeChild(targetBq);",
                "    }} else {{",
                "        const lastSel = selectedChildren[selectedChildren.length - 1];",
                "        const lastIdx = children.indexOf(lastSel);",
                "        const afterChildren = children.slice(lastIdx + 1);",
                "        let insertRef = targetBq.nextSibling;",
                "        if (afterChildren.length > 0) {{",
                "            const afterBq = targetBq.cloneNode(false);",
                "            afterChildren.forEach(function(c) {{ afterBq.appendChild(c); }});",
                "            parent.insertBefore(afterBq, insertRef);",
                "            insertRef = afterBq;",
                "        }}",
                "        selectedChildren.forEach(function(c) {{",
                "            if (c.nodeType === 3 && c.textContent.trim().length > 0) {{",
                "                const p = document.createElement('p');",
                "                p.appendChild(c);",
                "                parent.insertBefore(p, insertRef);",
                "            }} else {{",
                "                parent.insertBefore(c, insertRef);",
                "            }}",
                "        }});",
                "        if (!targetBq.hasChildNodes() || targetBq.textContent.trim() === '') {{",
                "            parent.removeChild(targetBq);",
                "        }}",
                "    }}",
                "    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    return true;",
                "}};",
                "window.meridianToggleBlockquote = function() {{",
                "    try {{",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (window.meridianUnwrapBlockquote()) return;",
                "        const sel = window.getSelection();",
                "        let blocks = meridianSelectedBlocks().filter(function(b) {{ return b.tagName !== 'BLOCKQUOTE'; }});",
                "        if (blocks.length === 0) {{",
                "            document.execCommand('formatBlock', false, 'p');",
                "            blocks = meridianSelectedBlocks().filter(function(b) {{ return b.tagName !== 'BLOCKQUOTE'; }});",
                "        }}",
                "        const bq = document.createElement('blockquote');",
                "        let targetP = null;",
                "        if (blocks.length > 0 && blocks[0].parentNode) {{",
                "            blocks[0].parentNode.insertBefore(bq, blocks[0]);",
                "            blocks.forEach(function(b) {{",
                "                if (b.tagName !== 'P' && b.tagName !== 'H1' && b.tagName !== 'H2' && b.tagName !== 'H3' && b.tagName !== 'PRE') {{",
                "                    const p = document.createElement('p');",
                "                    while (b.firstChild) p.appendChild(b.firstChild);",
                "                    if (isBlockEmpty(p)) p.innerHTML = '<br>';",
                "                    b.remove();",
                "                    bq.appendChild(p);",
                "                    if (!targetP) targetP = p;",
                "                }} else {{",
                "                    if (isBlockEmpty(b)) b.innerHTML = '<br>';",
                "                    bq.appendChild(b);",
                "                    if (!targetP) targetP = b;",
                "                }}",
                "            }});",
                "        }} else {{",
                "            const p = document.createElement('p');",
                "            p.innerHTML = '<br>';",
                "            bq.appendChild(p);",
                "            document.body.appendChild(bq);",
                "            targetP = p;",
                "        }}",
                "        while (bq.previousElementSibling && bq.previousElementSibling.tagName === 'BLOCKQUOTE') {{",
                "            const prevBq = bq.previousElementSibling;",
                "            while (prevBq.lastChild) bq.insertBefore(prevBq.lastChild, bq.firstChild);",
                "            prevBq.remove();",
                "        }}",
                "        while (bq.nextElementSibling && bq.nextElementSibling.tagName === 'BLOCKQUOTE') {{",
                "            const nextBq = bq.nextElementSibling;",
                "            while (nextBq.firstChild) bq.appendChild(nextBq.firstChild);",
                "            nextBq.remove();",
                "        }}",
                "        if (targetP && sel) {{",
                "            const cr = document.createRange();",
                "            if (isBlockEmpty(targetP)) {{",
                "                cr.setStart(targetP, 0);",
                "                cr.collapse(true);",
                "            }} else {{",
                "                cr.selectNodeContents(targetP);",
                "                cr.collapse(false);",
                "            }}",
                "            sel.removeAllRanges();",
                "            sel.addRange(cr);",
                "        }}",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    }} catch (err) {{}}",
                "}};",
                "function meridianSelectedBlocks() {{",
                "    const sel = window.getSelection();",
                "    if (!sel || sel.rangeCount === 0) return [];",
                "    const range = sel.getRangeAt(0);",
                "    let node = sel.anchorNode || range.startContainer;",
                "    if (node && node.nodeType === 3) node = node.parentElement;",
                "    const sig = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');",
                "    const candidates = Array.from(document.body.querySelectorAll('p, div, h1, h2, h3, pre, blockquote')).filter(function(el) {{",
                "        if (el === sig || (sig && sig.contains(el)) || el.id === '__meridian_drop_caret') return false;",
                "        if (el.tagName === 'DIV' && el.querySelector('p, div, h1, h2, h3, pre, blockquote')) return false;",
                "        return sel.containsNode(el, true) || (node && el.contains(node)) || el === node;",
                "    }});",
                "    if (candidates.length === 0 && node && node !== document.body) {{",
                "        const closest = node.closest ? node.closest('p, div, h1, h2, h3, pre, blockquote') : null;",
                "        if (closest && closest !== sig && !(sig && sig.contains(closest))) return [closest];",
                "    }}",
                "    return candidates;",
                "}};",
                "window.meridianIndent = function() {{",
                "    try {{",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        const sel = window.getSelection();",
                "        let node = sel && sel.rangeCount > 0 ? (sel.anchorNode || sel.getRangeAt(0).startContainer) : null;",
                "        if (node && node.nodeType === 3) node = node.parentElement;",
                "        if (node && node.closest && node.closest('li')) {{",
                "            document.execCommand('indent', false, null);",
                "        }} else {{",
                "            let blocks = meridianSelectedBlocks();",
                "            if (blocks.length === 0) {{",
                "                document.execCommand('formatBlock', false, 'p');",
                "                blocks = meridianSelectedBlocks();",
                "            }}",
                "            blocks.forEach(function(b) {{",
                "                const cur = parseInt(b.style.marginLeft || '0', 10) || 0;",
                "                b.style.marginLeft = Math.min(288, cur + 24) + 'px';",
                "            }});",
                "        }}",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    }} catch (err) {{}}",
                "}};",
                "window.meridianOutdent = function() {{",
                "    try {{",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        const sel = window.getSelection();",
                "        let node = sel && sel.rangeCount > 0 ? (sel.anchorNode || sel.getRangeAt(0).startContainer) : null;",
                "        if (node && node.nodeType === 3) node = node.parentElement;",
                "        if (node && node.closest && node.closest('li')) {{",
                "            document.execCommand('outdent', false, null);",
                "        }} else {{",
                "            const blocks = meridianSelectedBlocks();",
                "            let reduced = false;",
                "            blocks.forEach(function(b) {{",
                "                const cur = parseInt(b.style.marginLeft || '0', 10) || 0;",
                "                if (cur > 0) {{",
                "                    reduced = true;",
                "                    const next = Math.max(0, cur - 24);",
                "                    if (next === 0) b.style.removeProperty('margin-left');",
                "                    else b.style.marginLeft = next + 'px';",
                "                }}",
                "            }});",
                "            if (!reduced && node && node.closest && node.closest('blockquote')) {{",
                "                window.meridianUnwrapBlockquote();",
                "                return;",
                "            }}",
                "        }}",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    }} catch (err) {{}}",
                "}};",
                "window.__meridianActiveTable = null;",
                "window.meridianInsertTable = function(rows, cols) {{",
                "    try {{",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        const rCount = Math.max(1, Math.min(20, rows || 3));",
                "        const cCount = Math.max(1, Math.min(12, cols || 3));",
                "        const table = document.createElement('table');",
                "        table.setAttribute('style', 'border-collapse: collapse; width: calc(100% - 28px); margin: 6px 0 28px 0;');",
                "        const tbody = document.createElement('tbody');",
                "        let firstCell = null;",
                "        for (let r = 0; r < rCount; r++) {{",
                "            const tr = document.createElement('tr');",
                "            for (let c = 0; c < cCount; c++) {{",
                "                const td = document.createElement('td');",
                "                td.setAttribute('style', 'border: 1px solid #c0bfbc; padding: 6px 8px; vertical-align: top;');",
                "                td.innerHTML = '<br>';",
                "                if (!firstCell) firstCell = td;",
                "                tr.appendChild(td);",
                "            }}",
                "            tbody.appendChild(tr);",
                "        }}",
                "        table.appendChild(tbody);",
                "        const trailingP = document.createElement('p');",
                "        trailingP.innerHTML = '<br>';",
                "        const sel = window.getSelection();",
                "        const sig = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');",
                "        if (sel && sel.rangeCount > 0 && document.body.contains(sel.anchorNode)) {{",
                "            let range = sel.getRangeAt(0);",
                "            if (typeof constrainRangeBeforeSignature === 'function') range = constrainRangeBeforeSignature(range);",
                "            let block = (range.startContainer.nodeType === 1 ? range.startContainer : range.startContainer.parentElement);",
                "            block = block && block.closest ? block.closest('p, div, h1, h2, h3, pre, blockquote') : null;",
                "            if (block && block.parentNode === document.body && isBlockEmpty(block) && block !== sig) {{",
                "                document.body.insertBefore(table, block);",
                "            }} else if (block && block.parentNode === document.body && block !== sig) {{",
                "                document.body.insertBefore(table, block.nextSibling);",
                "                if (!table.nextElementSibling || table.nextElementSibling === sig) {{",
                "                    document.body.insertBefore(trailingP, table.nextSibling);",
                "                }}",
                "            }} else {{",
                "                range.deleteContents();",
                "                range.insertNode(trailingP);",
                "                range.insertNode(table);",
                "            }}",
                "        }} else if (sig) {{",
                "            document.body.insertBefore(table, sig);",
                "            document.body.insertBefore(trailingP, sig);",
                "        }} else {{",
                "            document.body.appendChild(table);",
                "            document.body.appendChild(trailingP);",
                "        }}",
                "        window.__meridianActiveTable = table;",
                "        if (firstCell && sel) {{",
                "            const cr = document.createRange();",
                "            cr.setStart(firstCell, 0);",
                "            cr.collapse(true);",
                "            sel.removeAllRanges();",
                "            sel.addRange(cr);",
                "        }}",
                "        if (typeof window.meridianUpdateTableUI === 'function') window.meridianUpdateTableUI();",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    }} catch (err) {{}}",
                "}};",
                "window.meridianModifyTable = function(action) {{",
                "    try {{",
                "        const sel = window.getSelection();",
                "        let node = sel && sel.rangeCount > 0 ? (sel.anchorNode || sel.getRangeAt(0).startContainer) : null;",
                "        if (node && node.nodeType === 3) node = node.parentElement;",
                "        let cell = node && node.closest ? node.closest('td, th') : null;",
                "        let table = (cell ? cell.closest('table') : null) || (window.__meridianActiveTable && document.body.contains(window.__meridianActiveTable) ? window.__meridianActiveTable : null) || document.body.querySelector('table');",
                "        if (!table) return;",
                "        if (cell && cell.closest('table') !== table) cell = null;",
                "        const row = cell ? cell.closest('tr') : null;",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (action === 'delTable') {{",
                "            table.remove();",
                "            window.__meridianActiveTable = null;",
                "        }} else if (action === 'addRow') {{",
                "            const targetRow = row || table.rows[table.rows.length - 1];",
                "            if (!targetRow) return;",
                "            const colCount = targetRow.cells.length || 1;",
                "            const newRow = table.insertRow(targetRow.rowIndex + 1);",
                "            for (let c = 0; c < colCount; c++) {{",
                "                const td = newRow.insertCell(c);",
                "                td.setAttribute('style', 'border: 1px solid #c0bfbc; padding: 6px 8px; vertical-align: top;');",
                "                td.innerHTML = '<br>';",
                "            }}",
                "        }} else if (action === 'addCol') {{",
                "            const insertIdx = cell ? (cell.cellIndex + 1) : -1;",
                "            for (let r = 0; r < table.rows.length; r++) {{",
                "                const tr = table.rows[r];",
                "                const td = tr.insertCell(insertIdx >= 0 && insertIdx <= tr.cells.length ? insertIdx : -1);",
                "                td.setAttribute('style', 'border: 1px solid #c0bfbc; padding: 6px 8px; vertical-align: top;');",
                "                td.innerHTML = '<br>';",
                "            }}",
                "        }} else if (action === 'delRow') {{",
                "            const targetRow = row || table.rows[table.rows.length - 1];",
                "            if (!targetRow) return;",
                "            if (table.rows.length <= 1) {{ table.remove(); window.__meridianActiveTable = null; }} else {{ table.deleteRow(targetRow.rowIndex); }}",
                "        }} else if (action === 'delCol') {{",
                "            const colIdx = cell ? cell.cellIndex : ((table.rows[0] ? table.rows[0].cells.length : 1) - 1);",
                "            if (colIdx < 0) return;",
                "            if (table.rows[0] && table.rows[0].cells.length <= 1) {{",
                "                table.remove();",
                "                window.__meridianActiveTable = null;",
                "            }} else {{",
                "                for (let r = 0; r < table.rows.length; r++) {{",
                "                    if (colIdx < table.rows[r].cells.length) table.rows[r].deleteCell(colIdx);",
                "                }}",
                "            }}",
                "        }}",
                "        if (typeof window.meridianUpdateTableUI === 'function') window.meridianUpdateTableUI();",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "    }} catch (err) {{}}",
                "}};",
                "(function() {{",
                "    let uiRoot = null;",
                "    let colPill = null;",
                "    let rowPill = null;",
                "    let resizingState = null;",
                "    let hoveringColPill = false;",
                "    let hoveringRowPill = false;",
                "    let showColNearEdge = false;",
                "    let showRowNearEdge = false;",
                "    const svgPlus = '<svg width=\"12\" height=\"12\" viewBox=\"0 0 12 12\" fill=\"none\" style=\"display:block;pointer-events:none;\"><path d=\"M6 2.25V9.75M2.25 6H9.75\" stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\"/></svg>';",
                "    const svgMinus = '<svg width=\"12\" height=\"12\" viewBox=\"0 0 12 12\" fill=\"none\" style=\"display:block;pointer-events:none;\"><path d=\"M2.5 6H9.5\" stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\"/></svg>';",
                "    const svgClose = '<svg width=\"12\" height=\"12\" viewBox=\"0 0 12 12\" fill=\"none\" style=\"display:block;pointer-events:none;\"><path d=\"M3.25 3.25L8.75 8.75M8.75 3.25L3.25 8.75\" stroke=\"currentColor\" stroke-width=\"1.7\" stroke-linecap=\"round\"/></svg>';",
                "    const btnCss = 'appearance:none;-webkit-appearance:none;border:none;background:transparent;color:#3d3846;width:20px;height:20px;padding:0;margin:0;cursor:pointer;display:flex;align-items:center;justify-content:center;border-radius:4px;user-select:none;';",
                "    const pillCss = 'position:fixed;z-index:2147483646;display:none;background:#f6f5f4;border:1px solid rgba(0,0,0,0.16);border-radius:6px;box-shadow:0 1px 4px rgba(0,0,0,0.10);padding:2px;user-select:none;-webkit-user-select:none;';",
                "    function makeStepBtn(svgHtml, title, action, extraColor) {{",
                "        const b = document.createElement('button');",
                "        b.type = 'button';",
                "        b.innerHTML = svgHtml;",
                "        b.title = title;",
                "        b.setAttribute('style', btnCss + (extraColor ? ('color:' + extraColor + ';') : ''));",
                "        b.addEventListener('mouseenter', function() {{ b.style.backgroundColor = 'rgba(0,0,0,0.08)'; }});",
                "        b.addEventListener('mouseleave', function() {{ b.style.backgroundColor = 'transparent'; }});",
                "        b.addEventListener('mousedown', function(e) {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            window.meridianModifyTable(action);",
                "        }});",
                "        return b;",
                "    }}",
                "    function ensureTableUI() {{",
                "        if (uiRoot && uiRoot.parentNode) return;",
                "        uiRoot = document.createElement('div');",
                "        uiRoot.id = '__meridian_table_ui';",
                "        uiRoot.setAttribute('contenteditable', 'false');",
                "        uiRoot.setAttribute('style', 'position:fixed;left:0;top:0;width:0;height:0;overflow:visible;z-index:2147483646;pointer-events:none;user-select:none;');",
                "        colPill = document.createElement('div');",
                "        colPill.setAttribute('style', pillCss + 'pointer-events:auto;flex-direction:column;gap:1px;');",
                "        colPill.appendChild(makeStepBtn(svgPlus, 'Add column', 'addCol'));",
                "        colPill.appendChild(makeStepBtn(svgMinus, 'Remove column', 'delCol'));",
                "        colPill.addEventListener('mouseenter', function() {{ hoveringColPill = true; window.meridianUpdateTableUI(); }});",
                "        colPill.addEventListener('mouseleave', function() {{ hoveringColPill = false; window.meridianUpdateTableUI(); }});",
                "        rowPill = document.createElement('div');",
                "        rowPill.setAttribute('style', pillCss + 'pointer-events:auto;flex-direction:row;align-items:center;gap:1px;');",
                "        rowPill.appendChild(makeStepBtn(svgPlus, 'Add row', 'addRow'));",
                "        rowPill.appendChild(makeStepBtn(svgMinus, 'Remove row', 'delRow'));",
                "        const sep = document.createElement('div');",
                "        sep.setAttribute('style', 'width:1px;height:12px;background:rgba(0,0,0,0.15);margin:0 2px;');",
                "        rowPill.appendChild(sep);",
                "        rowPill.appendChild(makeStepBtn(svgClose, 'Delete table', 'delTable', '#c01c28'));",
                "        rowPill.addEventListener('mouseenter', function() {{ hoveringRowPill = true; window.meridianUpdateTableUI(); }});",
                "        rowPill.addEventListener('mouseleave', function() {{ hoveringRowPill = false; window.meridianUpdateTableUI(); }});",
                "        uiRoot.appendChild(colPill);",
                "        uiRoot.appendChild(rowPill);",
                "        document.documentElement.appendChild(uiRoot);",
                "    }}",
                "    window.meridianUpdateTableUI = function() {{",
                "        try {{",
                "            if (window.__plainTextMode) {{",
                "                if (colPill) colPill.style.display = 'none';",
                "                if (rowPill) rowPill.style.display = 'none';",
                "                return;",
                "            }}",
                "            const sel = window.getSelection();",
                "            let node = sel && sel.rangeCount > 0 ? (sel.anchorNode || sel.getRangeAt(0).startContainer) : null;",
                "            if (node && node.nodeType === 3) node = node.parentElement;",
                "            const selTable = node && node.closest ? node.closest('table') : null;",
                "            if (selTable) window.__meridianActiveTable = selTable;",
                "            const table = window.__meridianActiveTable;",
                "            if (!table || !document.body || !document.body.contains(table)) {{",
                "                if (colPill) colPill.style.display = 'none';",
                "                if (rowPill) rowPill.style.display = 'none';",
                "                return;",
                "            }}",
                "            ensureTableUI();",
                "            const rect = table.getBoundingClientRect();",
                "            if (rect.width <= 0 || rect.height <= 0) {{",
                "                colPill.style.display = 'none';",
                "                rowPill.style.display = 'none';",
                "                return;",
                "            }}",
                "            const visCol = Boolean(showColNearEdge || hoveringColPill);",
                "            const visRow = Boolean(showRowNearEdge || hoveringRowPill);",
                "            if (visCol) {{",
                "                colPill.style.display = 'flex';",
                "                colPill.style.left = Math.min(window.innerWidth - 26, Math.round(rect.right + 4)) + 'px';",
                "                colPill.style.top = Math.max(4, Math.round(rect.top + Math.max(0, (rect.height - 44) / 2))) + 'px';",
                "            }} else {{",
                "                colPill.style.display = 'none';",
                "            }}",
                "            if (visRow) {{",
                "                rowPill.style.display = 'flex';",
                "                rowPill.style.left = Math.max(4, Math.round(rect.left)) + 'px';",
                "                rowPill.style.top = Math.round(rect.bottom + 4) + 'px';",
                "            }} else {{",
                "                rowPill.style.display = 'none';",
                "            }}",
                "        }} catch (e) {{}}",
                "    }};",
                "    document.addEventListener('mousedown', function(e) {{",
                "        if (uiRoot && uiRoot.contains(e.target)) return;",
                "        const cell = e.target && e.target.closest ? e.target.closest('td, th') : null;",
                "        if (cell) {{",
                "            const tbl = cell.closest('table');",
                "            window.__meridianActiveTable = tbl;",
                "            const cr = cell.getBoundingClientRect();",
                "            if (e.clientX >= cr.right - 5 && e.clientX <= cr.right + 3) {{",
                "                e.preventDefault();",
                "                if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "                resizingState = {{ table: tbl, colIndex: cell.cellIndex, startX: e.clientX, startWidth: cr.width }};",
                "            }}",
                "        }} else {{",
                "            const tbl = e.target && e.target.closest ? e.target.closest('table') : null;",
                "            window.__meridianActiveTable = tbl;",
                "            if (!tbl) {{ showColNearEdge = false; showRowNearEdge = false; }}",
                "        }}",
                "        setTimeout(window.meridianUpdateTableUI, 0);",
                "    }});",
                "    document.addEventListener('mousemove', function(e) {{",
                "        if (resizingState) {{",
                "            e.preventDefault();",
                "            const w = Math.max(36, Math.round(resizingState.startWidth + (e.clientX - resizingState.startX)));",
                "            const rows = resizingState.table.rows;",
                "            for (let r = 0; r < rows.length; r++) {{",
                "                if (resizingState.colIndex < rows[r].cells.length) {{",
                "                    rows[r].cells[resizingState.colIndex].style.width = w + 'px';",
                "                }}",
                "            }}",
                "            window.meridianUpdateTableUI();",
                "            return;",
                "        }}",
                "        const cell = e.target && e.target.closest ? e.target.closest('td, th') : null;",
                "        if (cell) {{",
                "            const cr = cell.getBoundingClientRect();",
                "            if (e.clientX >= cr.right - 5 && e.clientX <= cr.right + 3) {{",
                "                cell.style.cursor = 'col-resize';",
                "            }} else if (cell.style.cursor === 'col-resize') {{",
                "                cell.style.cursor = '';",
                "            }}",
                "        }}",
                "        let hoveredTable = (e.target && e.target.closest) ? e.target.closest('table') : null;",
                "        if (!hoveredTable && document.body) {{",
                "            const tables = document.body.querySelectorAll('table');",
                "            for (let i = 0; i < tables.length; i++) {{",
                "                const tr = tables[i].getBoundingClientRect();",
                "                if (e.clientX >= tr.left - 12 && e.clientX <= tr.right + 38 && e.clientY >= tr.top - 12 && e.clientY <= tr.bottom + 38) {{",
                "                    hoveredTable = tables[i];",
                "                    break;",
                "                }}",
                "            }}",
                "        }}",
                "        if (hoveredTable) {{",
                "            window.__meridianActiveTable = hoveredTable;",
                "            const r = hoveredTable.getBoundingClientRect();",
                "            showColNearEdge = (e.clientX >= r.right - 42 && e.clientX <= r.right + 38 && e.clientY >= r.top - 8 && e.clientY <= r.bottom + 8);",
                "            showRowNearEdge = (e.clientY >= r.bottom - 32 && e.clientY <= r.bottom + 38 && e.clientX >= r.left - 8 && e.clientX <= r.right + 28);",
                "        }} else if (!hoveringColPill && !hoveringRowPill) {{",
                "            showColNearEdge = false;",
                "            showRowNearEdge = false;",
                "        }}",
                "        window.meridianUpdateTableUI();",
                "    }});",
                "    document.addEventListener('keydown', function() {{",
                "        if (showColNearEdge || showRowNearEdge) {{",
                "            showColNearEdge = false;",
                "            showRowNearEdge = false;",
                "            window.meridianUpdateTableUI();",
                "        }}",
                "    }}, true);",
                "    document.addEventListener('mouseup', function() {{",
                "        if (resizingState) {{",
                "            resizingState = null;",
                "            if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "            if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "            window.meridianUpdateTableUI();",
                "        }}",
                "    }});",
                "    window.addEventListener('scroll', function() {{ window.meridianUpdateTableUI(); }}, true);",
                "    window.addEventListener('resize', function() {{ window.meridianUpdateTableUI(); }});",
                "}})();",
                "window.__meridianUndoStack = [];",
                "window.__meridianRedoStack = [];",
                "window.__meridianUserEdited = false;",
                "let __meridianLastSnapshotTime = 0;",
                "let __meridianInputTimer = null;",
                "function meridianSaveSelection() {{",
                "    try {{",
                "        const sel = window.getSelection();",
                "        if (!sel || sel.rangeCount === 0) return null;",
                "        const range = sel.getRangeAt(0);",
                "        const preStart = range.cloneRange();",
                "        preStart.selectNodeContents(document.body);",
                "        preStart.setEnd(range.startContainer, range.startOffset);",
                "        const start = preStart.toString().length;",
                "        const end = start + range.toString().length;",
                "        return {{ start: start, end: end }};",
                "    }} catch (e) {{ return null; }}",
                "}};",
                "function meridianRestoreSelection(saved) {{",
                "    if (!saved) return;",
                "    try {{",
                "        let charIndex = 0;",
                "        const range = document.createRange();",
                "        range.setStart(document.body, 0);",
                "        range.collapse(true);",
                "        const nodeStack = [document.body];",
                "        let node, foundStart = false, stop = false;",
                "        while (!stop && (node = nodeStack.pop())) {{",
                "            if (node.nodeType === 3) {{",
                "                const nextCharIndex = charIndex + node.length;",
                "                if (!foundStart && saved.start >= charIndex && saved.start <= nextCharIndex) {{",
                "                    range.setStart(node, saved.start - charIndex);",
                "                    foundStart = true;",
                "                }}",
                "                if (foundStart && saved.end >= charIndex && saved.end <= nextCharIndex) {{",
                "                    range.setEnd(node, saved.end - charIndex);",
                "                    stop = true;",
                "                }}",
                "                charIndex = nextCharIndex;",
                "            }} else {{",
                "                let i = node.childNodes.length;",
                "                while (i--) {{ nodeStack.push(node.childNodes[i]); }}",
                "            }}",
                "        }}",
                "        const sel = window.getSelection();",
                "        if (sel) {{",
                "            sel.removeAllRanges();",
                "            sel.addRange(range);",
                "        }}",
                "    }} catch (e) {{}}",
                "}};",
                "function meridianCaptureSnapshot() {{",
                "    return {{",
                "        html: document.body ? document.body.innerHTML : '',",
                "        bodyFont: (document.body && document.body.style) ? (document.body.style.fontFamily || '') : '',",
                "        bodySize: (document.body && document.body.style) ? (document.body.style.fontSize || '') : '',",
                "        sel: meridianSaveSelection()",
                "    }};",
                "}};",
                "function meridianSnapshotsEqual(a, b) {{",
                "    if (!a || !b) return false;",
                "    return a.html === b.html && a.bodyFont === b.bodyFont && a.bodySize === b.bodySize;",
                "}};",
                "function meridianApplySnapshot(snap) {{",
                "    if (!snap || !document.body) return;",
                "    document.body.innerHTML = snap.html;",
                "    document.body.style.fontFamily = snap.bodyFont || '';",
                "    document.body.style.fontSize = snap.bodySize || '';",
                "    meridianRestoreSelection(snap.sel);",
                "    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "}};",
                "window.meridianResetHistory = function() {{",
                "    if (__meridianInputTimer) {{ clearTimeout(__meridianInputTimer); __meridianInputTimer = null; }}",
                "    window.__meridianUndoStack = [meridianCaptureSnapshot()];",
                "    window.__meridianRedoStack = [];",
                "    window.__meridianUserEdited = false;",
                "    __meridianLastSnapshotTime = Date.now();",
                "}};",
                "window.meridianRecordUndoSnapshot = function(immediate) {{",
                "    if (!document.body) return;",
                "    const snap = meridianCaptureSnapshot();",
                "    const stack = window.__meridianUndoStack;",
                "    const top = stack.length > 0 ? stack[stack.length - 1] : null;",
                "    if (!top || !meridianSnapshotsEqual(top, snap)) {{",
                "        stack.push(snap);",
                "        if (stack.length > 100) stack.shift();",
                "        window.__meridianRedoStack = [];",
                "        __meridianLastSnapshotTime = Date.now();",
                "    }} else if (top && snap.sel) {{",
                "        top.sel = snap.sel;",
                "    }}",
                "    if (immediate) {{",
                "        window.__meridianUserEdited = true;",
                "    }}",
                "}};",
                "window.meridianUndo = function() {{",
                "    if (__meridianInputTimer) {{ clearTimeout(__meridianInputTimer); __meridianInputTimer = null; }}",
                "    const stack = window.__meridianUndoStack;",
                "    if (!stack || stack.length === 0) {{",
                "        document.execCommand('undo', false, null);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "        return;",
                "    }}",
                "    const current = meridianCaptureSnapshot();",
                "    const top = stack[stack.length - 1];",
                "    if (!meridianSnapshotsEqual(current, top)) {{",
                "        window.__meridianRedoStack.push(current);",
                "        meridianApplySnapshot(top);",
                "        return;",
                "    }}",
                "    if (stack.length > 1) {{",
                "        const popped = stack.pop();",
                "        window.__meridianRedoStack.push(popped);",
                "        meridianApplySnapshot(stack[stack.length - 1]);",
                "    }}",
                "}};",
                "window.meridianRedo = function() {{",
                "    if (__meridianInputTimer) {{ clearTimeout(__meridianInputTimer); __meridianInputTimer = null; }}",
                "    const redoStack = window.__meridianRedoStack;",
                "    if (!redoStack || redoStack.length === 0) {{",
                "        document.execCommand('redo', false, null);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "        return;",
                "    }}",
                "    const next = redoStack.pop();",
                "    window.__meridianUndoStack.push(next);",
                "    meridianApplySnapshot(next);",
                "}};",
                "window.__meridianCopiedStyle = null;",
                "window.meridianCopyStyle = function() {{",
                "    try {{",
                "        const sel = window.getSelection();",
                "        let node = null;",
                "        if (sel && sel.rangeCount > 0) {{",
                "            const r = sel.getRangeAt(0);",
                "            node = sel.focusNode || r.startContainer;",
                "            if (node && node.nodeType === 1 && sel.focusOffset > 0 && sel.focusOffset <= node.childNodes.length) {{",
                "                const candidate = node.childNodes[sel.focusOffset - 1];",
                "                if (candidate) node = candidate;",
                "            }}",
                "            while (node && node.nodeType !== 1) {{",
                "                node = node.parentElement || node.parentNode;",
                "            }}",
                "        }}",
                "        if (!node) node = document.body;",
                "        const cs = window.getComputedStyle ? window.getComputedStyle(node) : null;",
                "        const bold = Boolean(document.queryCommandState('bold')) || Boolean(node.closest && node.closest('b, strong, h1, h2, h3')) || Boolean(cs && (parseInt(cs.fontWeight, 10) >= 600 || cs.fontWeight === 'bold'));",
                "        const italic = Boolean(document.queryCommandState('italic')) || Boolean(node.closest && node.closest('i, em')) || Boolean(cs && (cs.fontStyle === 'italic' || cs.fontStyle === 'oblique'));",
                "        const underline = Boolean(document.queryCommandState('underline')) || Boolean(node.closest && node.closest('u'));",
                "        const strikethrough = Boolean(document.queryCommandState('strikethrough')) || Boolean(node.closest && node.closest('s, strike, del'));",
                "        let blockTag = 'p';",
                "        if (node.closest) {{",
                "            if (node.closest('h1')) blockTag = 'h1';",
                "            else if (node.closest('h2')) blockTag = 'h2';",
                "            else if (node.closest('h3')) blockTag = 'h3';",
                "            else if (node.closest('pre')) blockTag = 'pre';",
                "            else if (node.closest('blockquote')) blockTag = 'blockquote';",
                "        }}",
                "        let color = cs ? (cs.color || '') : '';",
                "        let fontFamily = cs ? (cs.fontFamily || '') : '';",
                "        let fontSize = cs ? (cs.fontSize || '') : '';",
                "        let textAlign = cs ? (cs.textAlign || '') : '';",
                "        let bg = '';",
                "        let el = node;",
                "        while (el && el !== document.body && el.nodeType === 1) {{",
                "            if (el.style && el.style.backgroundColor && el.style.backgroundColor !== 'transparent' && el.style.backgroundColor !== 'rgba(0, 0, 0, 0)') {{",
                "                bg = el.style.backgroundColor;",
                "                break;",
                "            }}",
                "            el = el.parentElement;",
                "        }}",
                "        window.__meridianCopiedStyle = {{",
                "            bold: bold,",
                "            italic: italic,",
                "            underline: underline,",
                "            strikethrough: strikethrough,",
                "            fontFamily: fontFamily,",
                "            fontSize: fontSize,",
                "            color: color,",
                "            backgroundColor: bg,",
                "            blockTag: blockTag,",
                "            textAlign: textAlign",
                "        }};",
                "        return true;",
                "    }} catch (e) {{ return false; }}",
                "}};",
                "window.meridianApplyCopiedStyle = function() {{",
                "    try {{",
                "        const st = window.__meridianCopiedStyle;",
                "        if (!st) return false;",
                "        const sel = window.getSelection();",
                "        if (!sel || sel.rangeCount === 0) return false;",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        document.execCommand('styleWithCSS', false, true);",
                "        document.execCommand('removeFormat', false, null);",
                "        if (st.blockTag && ['p', 'h1', 'h2', 'h3', 'pre', 'blockquote'].includes(st.blockTag)) {{",
                "            document.execCommand('formatBlock', false, st.blockTag);",
                "        }}",
                "        if (Boolean(document.queryCommandState('bold')) !== Boolean(st.bold)) {{",
                "            document.execCommand('bold', false, null);",
                "        }}",
                "        if (Boolean(document.queryCommandState('italic')) !== Boolean(st.italic)) {{",
                "            document.execCommand('italic', false, null);",
                "        }}",
                "        if (Boolean(document.queryCommandState('underline')) !== Boolean(st.underline)) {{",
                "            document.execCommand('underline', false, null);",
                "        }}",
                "        if (Boolean(document.queryCommandState('strikethrough')) !== Boolean(st.strikethrough)) {{",
                "            document.execCommand('strikethrough', false, null);",
                "        }}",
                "        if (st.fontFamily) {{",
                "            document.execCommand('fontName', false, st.fontFamily);",
                "        }}",
                "        if (st.color) {{",
                "            document.execCommand('foreColor', false, st.color);",
                "        }}",
                "        if (st.backgroundColor) {{",
                "            document.execCommand('hiliteColor', false, st.backgroundColor);",
                "        }}",
                "        if (st.fontSize) {{",
                "            window.__meridianSuppressSelectionNotify = true;",
                "            document.execCommand('styleWithCSS', false, false);",
                "            document.execCommand('fontSize', false, '7');",
                "            const spans = document.querySelectorAll('font[size=\"7\"], span[style*=\"xxx-large\"], [style*=\"-webkit-xxx-large\"]');",
                "            for (let i = 0; i < spans.length; i++) {{",
                "                const el = spans[i];",
                "                if (el.tagName === 'FONT') {{",
                "                    const sp = document.createElement('span');",
                "                    sp.style.fontSize = st.fontSize;",
                "                    while (el.firstChild) sp.appendChild(el.firstChild);",
                "                    if (el.parentNode) el.parentNode.replaceChild(sp, el);",
                "                }} else {{",
                "                    el.removeAttribute('size');",
                "                    el.style.fontSize = st.fontSize;",
                "                }}",
                "            }}",
                "            window.__meridianSuppressSelectionNotify = false;",
                "        }}",
                "        if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "        return true;",
                "    }} catch (e) {{ return false; }}",
                "}};",
                "window.meridianInsertPlainTextMatchStyle = function(text) {{",
                "    if (typeof text !== 'string' || text.length === 0) return;",
                "    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "    document.execCommand('insertText', false, text);",
                "    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);",
                "    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "}};",
                "document.addEventListener('beforeinput', function(e) {{",
                "    window.__meridianUserEdited = true;",
                "    const now = Date.now();",
                "    const isWordBreak = e.data === ' ' || e.data === '\\n' || e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak';",
                "    if (window.__meridianUndoStack.length === 0 || isWordBreak || e.inputType !== 'insertText' || (now - __meridianLastSnapshotTime > 400)) {{",
                "        window.meridianRecordUndoSnapshot(false);",
                "    }}",
                "    if (!window.__plainTextMode && window.__meridianAutoFormatEnabled !== false && e.inputType === 'insertParagraph') {{",
                "        try {{",
                "            const sel = window.getSelection();",
                "            if (sel && sel.isCollapsed && sel.rangeCount > 0) {{",
                "                const r = sel.getRangeAt(0);",
                "                const node = r.startContainer;",
                "                const parentEl = node.nodeType === 1 ? node : node.parentElement;",
                "                if (parentEl && !parentEl.closest('pre, code')) {{",
                "                    const block = parentEl.closest('p, div') || document.body;",
                "                    if ((block.textContent || '').trim() === '---') {{",
                "                        e.preventDefault();",
                "                        window.meridianRecordUndoSnapshot(true);",
                "                        const hr = document.createElement('hr');",
                "                        const nextP = document.createElement('p');",
                "                        nextP.innerHTML = '<br>';",
                "                        if (block && block !== document.body && block.parentNode) {{",
                "                            block.parentNode.insertBefore(hr, block);",
                "                            block.parentNode.replaceChild(nextP, block);",
                "                        }} else {{",
                "                            block.innerHTML = '';",
                "                            document.execCommand('insertHorizontalRule', false, null);",
                "                        }}",
                "                        if (nextP.parentNode) {{",
                "                            const nr = document.createRange();",
                "                            nr.setStart(nextP, 0);",
                "                            nr.collapse(true);",
                "                            sel.removeAllRanges();",
                "                            sel.addRange(nr);",
                "                        }}",
                "                        window.__meridianJustAutoFormatted = true;",
                "                        window.meridianRecordUndoSnapshot(true);",
                "                        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                        return;",
                "                    }}",
                "                }}",
                "            }}",
                "        }} catch (err) {{}}",
                "    }}",
                "    if (!window.__plainTextMode && window.__meridianAutoFormatEnabled !== false && e.inputType === 'insertText') {{",
                "        try {{",
                "            const sel = window.getSelection();",
                "            if (sel && sel.isCollapsed && sel.rangeCount > 0) {{",
                "                const r = sel.getRangeAt(0);",
                "                const node = r.startContainer;",
                "                const parentEl = node.nodeType === 1 ? node : node.parentElement;",
                "                if (parentEl && !parentEl.closest('pre, code')) {{",
                "                    if (e.data === ' ' && node.nodeType === 3) {{",
                "                        const block = parentEl.closest('p, div, h1, h2, h3, li') || document.body;",
                "                        const preRange = document.createRange();",
                "                        preRange.selectNodeContents(block);",
                "                        preRange.setEnd(node, r.startOffset);",
                "                        const prefix = preRange.toString().replace(/\\u200b/g, '');",
                "                        let mdAction = null;",
                "                        if (prefix === '-' || prefix === '*') mdAction = 'ul';",
                "                        else if (prefix === '1.') mdAction = 'ol';",
                "                        else if (prefix === '>') mdAction = 'blockquote';",
                "                        else if (prefix === '#') mdAction = 'h1';",
                "                        else if (prefix === '##') mdAction = 'h2';",
                "                        else if (prefix === '###') mdAction = 'h3';",
                "                        if (mdAction) {{",
                "                            e.preventDefault();",
                "                            window.meridianRecordUndoSnapshot(true);",
                "                            preRange.deleteContents();",
                "                            if (mdAction === 'h1' || mdAction === 'h2' || mdAction === 'h3') {{",
                "                                const heading = document.createElement(mdAction);",
                "                                if (block && block !== document.body && block.parentNode && block.tagName !== 'LI') {{",
                "                                    while (block.firstChild) heading.appendChild(block.firstChild);",
                "                                    if (isBlockEmpty(heading)) heading.innerHTML = '<br>';",
                "                                    block.parentNode.replaceChild(heading, block);",
                "                                }} else {{",
                "                                    heading.innerHTML = '<br>';",
                "                                    r.insertNode(heading);",
                "                                }}",
                "                                const nr = document.createRange();",
                "                                nr.setStart(heading, 0);",
                "                                nr.collapse(true);",
                "                                sel.removeAllRanges();",
                "                                sel.addRange(nr);",
                "                            }} else if (mdAction === 'ul' || mdAction === 'ol') {{",
                "                                const listEl = document.createElement(mdAction);",
                "                                const li = document.createElement('li');",
                "                                if (block && block !== document.body && block.parentNode && block.tagName !== 'LI') {{",
                "                                    while (block.firstChild) li.appendChild(block.firstChild);",
                "                                    if (isBlockEmpty(li)) li.innerHTML = '<br>';",
                "                                    listEl.appendChild(li);",
                "                                    block.parentNode.replaceChild(listEl, block);",
                "                                }} else {{",
                "                                    li.innerHTML = '<br>';",
                "                                    listEl.appendChild(li);",
                "                                    r.insertNode(listEl);",
                "                                }}",
                "                                const nr = document.createRange();",
                "                                nr.setStart(li, 0);",
                "                                nr.collapse(true);",
                "                                sel.removeAllRanges();",
                "                                sel.addRange(nr);",
                "                            }} else if (mdAction === 'blockquote') {{",
                "                                if (block && block !== document.body && isBlockEmpty(block)) {{",
                "                                    block.innerHTML = '<br>';",
                "                                    const brRange = document.createRange();",
                "                                    brRange.setStart(block, 0);",
                "                                    brRange.collapse(true);",
                "                                    sel.removeAllRanges();",
                "                                    sel.addRange(brRange);",
                "                                }}",
                "                                if (typeof window.meridianToggleBlockquote === 'function') window.meridianToggleBlockquote();",
                "                            }}",
                "                            window.__meridianJustAutoFormatted = true;",
                "                            window.meridianRecordUndoSnapshot(true);",
                "                            if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                            if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                            return;",
                "                        }}",
                "                    }} else if ((e.data === '`' || e.data === '*' || e.data === '~') && node.nodeType === 3) {{",
                "                        const textBefore = (node.nodeValue || '').slice(0, r.startOffset);",
                "                        let openIdx = -1;",
                "                        let markerLen = 1;",
                "                        let tagName = null;",
                "                        let deleteEndOffset = r.startOffset;",
                "                        if (e.data === '`') {{",
                "                            openIdx = textBefore.lastIndexOf('`');",
                "                            markerLen = 1;",
                "                            tagName = 'code';",
                "                        }} else if (e.data === '*' && textBefore.endsWith('*')) {{",
                "                            const searchArea = textBefore.slice(0, -1);",
                "                            const dblIdx = searchArea.lastIndexOf('**');",
                "                            if (dblIdx !== -1 && dblIdx < searchArea.length - 2) {{",
                "                                openIdx = dblIdx;",
                "                                markerLen = 2;",
                "                                tagName = 'strong';",
                "                            }}",
                "                        }} else if (e.data === '*' && !textBefore.endsWith('*')) {{",
                "                            const singleIdx = textBefore.lastIndexOf('*');",
                "                            if (singleIdx !== -1 && singleIdx < r.startOffset - 1 && (singleIdx === 0 || textBefore[singleIdx - 1] !== '*')) {{",
                "                                openIdx = singleIdx;",
                "                                markerLen = 1;",
                "                                tagName = 'em';",
                "                            }}",
                "                        }} else if (e.data === '~' && textBefore.endsWith('~')) {{",
                "                            const searchArea = textBefore.slice(0, -1);",
                "                            const dblIdx = searchArea.lastIndexOf('~~');",
                "                            if (dblIdx !== -1 && dblIdx < searchArea.length - 2) {{",
                "                                openIdx = dblIdx;",
                "                                markerLen = 2;",
                "                                tagName = 's';",
                "                            }}",
                "                        }}",
                "                        if (tagName && openIdx !== -1) {{",
                "                            const innerEnd = markerLen === 2 ? (r.startOffset - 1) : r.startOffset;",
                "                            const innerText = textBefore.slice(openIdx + markerLen, innerEnd);",
                "                            if (innerText.trim().length > 0) {{",
                "                                e.preventDefault();",
                "                                window.meridianRecordUndoSnapshot(true);",
                "                                const inlineRange = document.createRange();",
                "                                inlineRange.setStart(node, openIdx);",
                "                                inlineRange.setEnd(node, deleteEndOffset);",
                "                                inlineRange.deleteContents();",
                "                                const inlineEl = document.createElement(tagName);",
                "                                inlineEl.textContent = innerText;",
                "                                const zwspNode = document.createTextNode('\\u200b');",
                "                                inlineRange.insertNode(zwspNode);",
                "                                inlineRange.insertNode(inlineEl);",
                "                                const afterRange = document.createRange();",
                "                                afterRange.setStartAfter(zwspNode);",
                "                                afterRange.collapse(true);",
                "                                sel.removeAllRanges();",
                "                                sel.addRange(afterRange);",
                "                                window.__meridianJustAutoFormatted = true;",
                "                                window.meridianRecordUndoSnapshot(true);",
                "                                if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                                if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                                return;",
                "                            }}",
                "                        }}",
                "                    }}",
                "                }}",
                "            }}",
                "        }} catch (err) {{}}",
                "    }}",
                "    window.__meridianJustAutoFormatted = false;",
                "}});",
                "document.addEventListener('input', function() {{",
                "    window.__meridianUserEdited = true;",
                "    if (__meridianInputTimer) clearTimeout(__meridianInputTimer);",
                "    __meridianInputTimer = setTimeout(function() {{",
                "        __meridianInputTimer = null;",
                "        window.meridianRecordUndoSnapshot(false);",
                "    }}, 250);",
                "}});",
                "document.addEventListener('keydown', function(e) {{",
                "    if (e.key === 'Backspace' && !e.ctrlKey && !e.metaKey && !e.altKey && window.__meridianJustAutoFormatted) {{",
                "        window.__meridianJustAutoFormatted = false;",
                "        e.preventDefault();",
                "        e.stopPropagation();",
                "        window.meridianUndo();",
                "        return;",
                "    }} else if (e.key !== 'Shift' && e.key !== 'Control' && e.key !== 'Alt' && e.key !== 'Meta') {{",
                "        window.__meridianJustAutoFormatted = false;",
                "    }}",
                "    if (e.key === 'Backspace' && !e.ctrlKey && !e.metaKey && !e.altKey && !window.__plainTextMode) {{",
                "        const sel = window.getSelection();",
                "        if (sel && sel.isCollapsed && sel.rangeCount > 0) {{",
                "            const r = sel.getRangeAt(0);",
                "            let node = r.startContainer;",
                "            if (node && node.nodeType === 3) node = node.parentElement;",
                "            const bq = node && node.closest ? node.closest('blockquote') : null;",
                "            if (bq && !(node && node.closest && node.closest('table, pre, ul, ol'))) {{",
                "                while (bq.previousElementSibling && bq.previousElementSibling.tagName === 'BLOCKQUOTE') {{",
                "                    const prevBq = bq.previousElementSibling;",
                "                    while (prevBq.lastChild) bq.insertBefore(prevBq.lastChild, bq.firstChild);",
                "                    prevBq.remove();",
                "                }}",
                "                while (bq.nextElementSibling && bq.nextElementSibling.tagName === 'BLOCKQUOTE') {{",
                "                    const nextBq = bq.nextElementSibling;",
                "                    while (nextBq.firstChild) bq.appendChild(nextBq.firstChild);",
                "                    nextBq.remove();",
                "                }}",
                "                let lineP = node && node.closest ? node.closest('p, div') : null;",
                "                if (!lineP || !bq.contains(lineP)) {{",
                "                    const wrapperP = document.createElement('p');",
                "                    while (bq.firstChild) wrapperP.appendChild(bq.firstChild);",
                "                    bq.appendChild(wrapperP);",
                "                    lineP = wrapperP;",
                "                }}",
                "                if (lineP === bq.firstElementChild) {{",
                "                    let atStart = isBlockEmpty(lineP);",
                "                    if (!atStart) {{",
                "                        try {{",
                "                            const pre = document.createRange();",
                "                            pre.selectNodeContents(lineP);",
                "                            pre.setEnd(r.startContainer, r.startOffset);",
                "                            atStart = pre.toString().replace(/\\u200b/g, '').length === 0;",
                "                        }} catch (err) {{}}",
                "                    }}",
                "                    if (atStart) {{",
                "                        e.preventDefault();",
                "                        e.stopPropagation();",
                "                        window.meridianRecordUndoSnapshot(true);",
                "                        const outP = document.createElement('p');",
                "                        if (isBlockEmpty(lineP)) {{",
                "                            outP.innerHTML = '<br>';",
                "                        }} else {{",
                "                            while (lineP.firstChild) outP.appendChild(lineP.firstChild);",
                "                        }}",
                "                        const parent = bq.parentNode || document.body;",
                "                        parent.insertBefore(outP, bq);",
                "                        lineP.remove();",
                "                        if (bq.children.length === 0 || isBlockEmpty(bq)) {{",
                "                            bq.remove();",
                "                        }}",
                "                        const nr = document.createRange();",
                "                        nr.setStart(outP, 0);",
                "                        nr.collapse(true);",
                "                        sel.removeAllRanges();",
                "                        sel.addRange(nr);",
                "                        window.meridianRecordUndoSnapshot(true);",
                "                        if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                        if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                        return;",
                "                    }}",
                "                }}",
                "            }}",
                "            const headingBlock = node && node.closest ? node.closest('h1, h2, h3, pre') : null;",
                "            if (headingBlock && !(node && node.closest && node.closest('table, ul, ol'))) {{",
                "                let atHeadingStart = isBlockEmpty(headingBlock);",
                "                if (!atHeadingStart) {{",
                "                    try {{",
                "                        const pre = document.createRange();",
                "                        pre.selectNodeContents(headingBlock);",
                "                        pre.setEnd(r.startContainer, r.startOffset);",
                "                        atHeadingStart = pre.toString().replace(/\\u200b/g, '').length === 0;",
                "                    }} catch (err) {{}}",
                "                }}",
                "                if (atHeadingStart) {{",
                "                    e.preventDefault();",
                "                    e.stopPropagation();",
                "                    window.meridianRecordUndoSnapshot(true);",
                "                    const outP = document.createElement('p');",
                "                    if (isBlockEmpty(headingBlock)) {{",
                "                        outP.innerHTML = '<br>';",
                "                    }} else {{",
                "                        while (headingBlock.firstChild) outP.appendChild(headingBlock.firstChild);",
                "                    }}",
                "                    if (headingBlock.parentNode) {{",
                "                        headingBlock.parentNode.replaceChild(outP, headingBlock);",
                "                    }}",
                "                    const nr = document.createRange();",
                "                    nr.setStart(outP, 0);",
                "                    nr.collapse(true);",
                "                    sel.removeAllRanges();",
                "                    sel.addRange(nr);",
                "                    window.meridianRecordUndoSnapshot(true);",
                "                    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                    return;",
                "                }}",
                "            }}",
                "        }}",
                "    }}",
                "    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey && !window.__plainTextMode) {{",
                "        const sel = window.getSelection();",
                "        if (sel && sel.rangeCount > 0) {{",
                "            const r = sel.getRangeAt(0);",
                "            let node = r.startContainer;",
                "            if (node && node.nodeType === 3) node = node.parentElement;",
                "            const headingBlock = node && node.closest ? node.closest('h1, h2, h3') : null;",
                "            if (headingBlock && !(node && node.closest && node.closest('table, pre, ul, ol, blockquote'))) {{",
                "                e.preventDefault();",
                "                e.stopPropagation();",
                "                window.meridianRecordUndoSnapshot(true);",
                "                const parent = headingBlock.parentNode || document.body;",
                "                if (isBlockEmpty(headingBlock)) {{",
                "                    const normalP = document.createElement('p');",
                "                    normalP.innerHTML = '<br>';",
                "                    parent.replaceChild(normalP, headingBlock);",
                "                    const nr = document.createRange();",
                "                    nr.setStart(normalP, 0);",
                "                    nr.collapse(true);",
                "                    sel.removeAllRanges();",
                "                    sel.addRange(nr);",
                "                }} else {{",
                "                    r.deleteContents();",
                "                    const tailRange = document.createRange();",
                "                    tailRange.selectNodeContents(headingBlock);",
                "                    tailRange.setStart(r.endContainer, r.endOffset);",
                "                    const tailFrag = tailRange.extractContents();",
                "                    const nextP = document.createElement('p');",
                "                    nextP.appendChild(tailFrag);",
                "                    if (isBlockEmpty(nextP)) nextP.innerHTML = '<br>';",
                "                    if (isBlockEmpty(headingBlock)) headingBlock.innerHTML = '<br>';",
                "                    parent.insertBefore(nextP, headingBlock.nextSibling);",
                "                    const nr = document.createRange();",
                "                    nr.setStart(nextP, 0);",
                "                    nr.collapse(true);",
                "                    sel.removeAllRanges();",
                "                    sel.addRange(nr);",
                "                }}",
                "                window.meridianRecordUndoSnapshot(true);",
                "                if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                return;",
                "            }}",
                "            const bq = node && node.closest ? node.closest('blockquote') : null;",
                "            if (bq && !(node && node.closest && node.closest('table, pre, ul, ol'))) {{",
                "                e.preventDefault();",
                "                e.stopPropagation();",
                "                window.meridianRecordUndoSnapshot(true);",
                "                while (bq.previousElementSibling && bq.previousElementSibling.tagName === 'BLOCKQUOTE') {{",
                "                    const prevBq = bq.previousElementSibling;",
                "                    while (prevBq.lastChild) bq.insertBefore(prevBq.lastChild, bq.firstChild);",
                "                    prevBq.remove();",
                "                }}",
                "                while (bq.nextElementSibling && bq.nextElementSibling.tagName === 'BLOCKQUOTE') {{",
                "                    const nextBq = bq.nextElementSibling;",
                "                    while (nextBq.firstChild) bq.appendChild(nextBq.firstChild);",
                "                    nextBq.remove();",
                "                }}",
                "                let lineP = node && node.closest ? node.closest('p, div') : null;",
                "                if (!lineP || !bq.contains(lineP)) {{",
                "                    const wrapperP = document.createElement('p');",
                "                    while (bq.firstChild) wrapperP.appendChild(bq.firstChild);",
                "                    bq.appendChild(wrapperP);",
                "                    lineP = wrapperP;",
                "                }}",
                "                if (isBlockEmpty(lineP)) {{",
                "                    const exitP = document.createElement('p');",
                "                    exitP.innerHTML = '<br>';",
                "                    const parent = bq.parentNode || document.body;",
                "                    const children = Array.from(bq.children);",
                "                    if (children.length <= 1) {{",
                "                        parent.replaceChild(exitP, bq);",
                "                    }} else if (lineP === bq.lastElementChild) {{",
                "                        lineP.remove();",
                "                        parent.insertBefore(exitP, bq.nextSibling);",
                "                    }} else if (lineP === bq.firstElementChild) {{",
                "                        lineP.remove();",
                "                        parent.insertBefore(exitP, bq);",
                "                    }} else {{",
                "                        const idx = children.indexOf(lineP);",
                "                        const afterBq = bq.cloneNode(false);",
                "                        children.slice(idx + 1).forEach(function(c) {{ afterBq.appendChild(c); }});",
                "                        lineP.remove();",
                "                        parent.insertBefore(afterBq, bq.nextSibling);",
                "                        parent.insertBefore(exitP, afterBq);",
                "                    }}",
                "                    const nr = document.createRange();",
                "                    nr.setStart(exitP, 0);",
                "                    nr.collapse(true);",
                "                    sel.removeAllRanges();",
                "                    sel.addRange(nr);",
                "                }} else {{",
                "                    r.deleteContents();",
                "                    const tailRange = document.createRange();",
                "                    tailRange.selectNodeContents(lineP);",
                "                    tailRange.setStart(r.endContainer, r.endOffset);",
                "                    const tailFrag = tailRange.extractContents();",
                "                    const nextP = document.createElement('p');",
                "                    nextP.appendChild(tailFrag);",
                "                    if (isBlockEmpty(nextP)) nextP.innerHTML = '<br>';",
                "                    if (isBlockEmpty(lineP)) lineP.innerHTML = '<br>';",
                "                    bq.insertBefore(nextP, lineP.nextSibling);",
                "                    const nr = document.createRange();",
                "                    nr.setStart(nextP, 0);",
                "                    nr.collapse(true);",
                "                    sel.removeAllRanges();",
                "                    sel.addRange(nr);",
                "                }}",
                "                window.meridianRecordUndoSnapshot(true);",
                "                if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                "                if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                "                return;",
                "            }}",
                "        }}",
                "    }}",
                "    if (e.key === 'Tab' && !e.ctrlKey && !e.metaKey && !e.altKey && !window.__plainTextMode) {{",
                "        const sel = window.getSelection();",
                "        if (sel && sel.rangeCount > 0) {{",
                "            let node = sel.anchorNode;",
                "            if (node && node.nodeType === 3) node = node.parentElement;",
                "            const td = node && node.closest ? node.closest('td, th') : null;",
                "            if (td) {{",
                "                e.preventDefault();",
                "                e.stopPropagation();",
                "                const table = td.closest('table');",
                "                if (table) {{",
                "                    const cells = Array.from(table.querySelectorAll('td, th'));",
                "                    const idx = cells.indexOf(td);",
                "                    let targetCell = null;",
                "                    if (e.shiftKey) {{",
                "                        if (idx > 0) targetCell = cells[idx - 1];",
                "                    }} else if (idx >= 0 && idx + 1 < cells.length) {{",
                "                        targetCell = cells[idx + 1];",
                "                    }} else if (!e.shiftKey && idx === cells.length - 1) {{",
                "                        window.meridianModifyTable('addRow');",
                "                        const updated = Array.from(table.querySelectorAll('td, th'));",
                "                        if (idx + 1 < updated.length) targetCell = updated[idx + 1];",
                "                    }}",
                "                    if (targetCell) {{",
                "                        const r = document.createRange();",
                "                        r.selectNodeContents(targetCell);",
                "                        r.collapse(true);",
                "                        sel.removeAllRanges();",
                "                        sel.addRange(r);",
                "                    }}",
                "                }}",
                "                return;",
                "            }}",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            if (e.shiftKey) {{",
                "                window.meridianOutdent();",
                "            }} else {{",
                "                window.meridianIndent();",
                "            }}",
                "            return;",
                "        }}",
                "    }}",
                "    if ((e.ctrlKey || e.metaKey) && !e.altKey) {{",
                "        const k = (e.key || '').toLowerCase();",
                "        if (k === 'z') {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            if (e.shiftKey) {{ window.meridianRedo(); }} else {{ window.meridianUndo(); }}",
                "        }} else if (k === 'y' && !e.shiftKey) {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            window.meridianRedo();",
                "        }} else if (k === 'c' && e.shiftKey) {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            window.meridianCopyStyle();",
                "        }}",
                "    }} else if ((e.ctrlKey || e.metaKey) && e.altKey && !e.shiftKey) {{",
                "        const k = (e.key || '').toLowerCase();",
                "        if (k === 'c') {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            window.meridianCopyStyle();",
                "        }} else if (k === 'v') {{",
                "            e.preventDefault();",
                "            e.stopPropagation();",
                "            window.meridianApplyCopiedStyle();",
                "        }}",
                "    }}",
                "}}, true);",
                "window.addEventListener('load', function() {{",
                "    if (window.__meridianUndoStack.length === 0) window.meridianResetHistory();",
                "}});",
                "function notifyMeridianSelectionState() {{",
                "    try {{",
                "        if (typeof window.meridianUpdateTableUI === 'function') window.meridianUpdateTableUI();",
                "        if (window.__meridianSuppressSelectionNotify) return;",
                "        if (!window.webkit || !window.webkit.messageHandlers || !window.webkit.messageHandlers.meridianSelectionState) return;",
                "        const sel = window.getSelection();",
                "        const hasSelection = Boolean(sel && !sel.isCollapsed && sel.toString().trim().length > 0);",
                "        let node = null;",
                "        if (sel && sel.rangeCount > 0) {{",
                "            const r = sel.getRangeAt(0);",
                "            node = sel.focusNode || r.startContainer;",
                "            if (node && node.nodeType === 1 && sel.focusOffset > 0 && sel.focusOffset <= node.childNodes.length) {{",
                "                const candidate = node.childNodes[sel.focusOffset - 1];",
                "                if (candidate) node = candidate;",
                "            }} else if (node && node.nodeType === 1 && sel.focusOffset === 0 && node.childNodes.length > 0) {{",
                "                const first = node.childNodes[0];",
                "                if (first && first.nodeType === 1 && first.tagName !== 'BR') node = first;",
                "            }}",
                "            while (node && node.nodeType !== 1) {{",
                "                node = node.parentElement || node.parentNode;",
                "            }}",
                "        }}",
                "        if (node === document.body && document.body && document.body.firstElementChild) {{",
                "            node = document.body.firstElementChild;",
                "        }}",
                "        const inBlockquote = Boolean(node && node.closest && node.closest('blockquote'));",
                "        let blockTag = 'p';",
                "        if (node && node.closest) {{",
                "            if (node.closest('h1')) blockTag = 'h1';",
                "            else if (node.closest('h2')) blockTag = 'h2';",
                "            else if (node.closest('h3')) blockTag = 'h3';",
                "            else if (node.closest('pre')) blockTag = 'pre';",
                "        }}",
                "        let fontFamily = '';",
                "        let fontSize = 0;",
                "        let cs = null;",
                "        if (node && window.getComputedStyle) {{",
                "            cs = window.getComputedStyle(node);",
                "            fontFamily = cs.fontFamily || '';",
                "            fontSize = Math.round(parseFloat(cs.fontSize) || 0);",
                "        }}",
                "        const isBold = Boolean(document.queryCommandState('bold')) || Boolean(node && node.closest && node.closest('b, strong, h1, h2, h3')) || Boolean(cs && (parseInt(cs.fontWeight, 10) >= 600 || cs.fontWeight === 'bold'));",
                "        const isItalic = Boolean(document.queryCommandState('italic')) || Boolean(node && node.closest && node.closest('i, em')) || Boolean(cs && (cs.fontStyle === 'italic' || cs.fontStyle === 'oblique'));",
                "        const isUnderline = Boolean(document.queryCommandState('underline')) || Boolean(node && node.closest && node.closest('u'));",
                "        const isStrike = Boolean(document.queryCommandState('strikethrough')) || Boolean(node && node.closest && node.closest('s, strike, del'));",
                "        const isUl = Boolean(document.queryCommandState('insertUnorderedList')) || Boolean(node && node.closest && node.closest('ul'));",
                "        const isOl = Boolean(document.queryCommandState('insertOrderedList')) || Boolean(node && node.closest && node.closest('ol'));",
                "        window.webkit.messageHandlers.meridianSelectionState.postMessage(JSON.stringify({{",
                "            has_selection: hasSelection,",
                "            bold: isBold,",
                "            italic: isItalic,",
                "            underline: isUnderline,",
                "            strikethrough: isStrike,",
                "            ul: isUl,",
                "            ol: isOl,",
                "            blockquote: inBlockquote,",
                "            block_tag: blockTag,",
                "            font_family: fontFamily,",
                "            font_size: fontSize",
                "        }}));",
                "    }} catch(err) {{}}",
                "}};",
                "window.notifyMeridianSelectionState = notifyMeridianSelectionState;",
                "document.addEventListener('selectionchange', notifyMeridianSelectionState);",
                "document.addEventListener('keyup', notifyMeridianSelectionState);",
                "document.addEventListener('mouseup', notifyMeridianSelectionState);",
                "function notifyMeridianContentChanged() {{",
                "    try {{",
                "        if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.meridianContentChanged) {{",
                "            window.webkit.messageHandlers.meridianContentChanged.postMessage(JSON.stringify({{",
                "                html: document.body.innerHTML,",
                "                text: meridianExtractPlainText(document.body)",
                "            }}));",
                "        }}",
                "    }} catch(err) {{}}",
                "}};",
                "window.notifyMeridianContentChanged = notifyMeridianContentChanged;",
                "document.addEventListener('input', notifyMeridianContentChanged);",
                "</script></head><body contenteditable='true'><p><br></p></body></html>"
            ),
            html_theme_class,
            font_family,
            font_size,
            initial_custom_css,
            initial_custom_map_json
        );

        let editor_weak_for_policy = self.downgrade();
        web_view.connect_decide_policy(move |_, decision, decision_type| {
            if decision_type == webkit6::PolicyDecisionType::NavigationAction
                || decision_type == webkit6::PolicyDecisionType::NewWindowAction
            {
                if let Ok(nav) = decision
                    .clone()
                    .downcast::<webkit6::NavigationPolicyDecision>()
                {
                    if let Some(mut action) = nav.navigation_action() {
                        if let Some(req) = action.request() {
                            if let Some(uri) = req.uri() {
                                if uri.starts_with("http://")
                                    || uri.starts_with("https://")
                                    || uri.starts_with("mailto:")
                                {
                                    let _ = gio::AppInfo::launch_default_for_uri(
                                        &uri,
                                        None::<&gio::AppLaunchContext>,
                                    );
                                    decision.ignore();
                                    return true;
                                } else if uri.starts_with("file://") {
                                    let file = gio::File::for_uri(&uri);
                                    if let Some(path) = file.path() {
                                        if path.is_file() {
                                            if let Some(editor) = editor_weak_for_policy.upgrade() {
                                                editor.dispatch_files_dropped(vec![path]);
                                            }
                                        }
                                    }
                                    decision.ignore();
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            false
        });

        let drop_target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop_target.set_types(&[gdk::FileList::static_type(), gio::File::static_type()]);

        drop_target.connect_enter(move |_, _, _| {
            gdk::DragAction::COPY
        });

        let web_view_for_leave = web_view.clone();
        drop_target.connect_leave(move |_| {
            web_view_for_leave.evaluate_javascript(
                "hideCaret(); currentDropRange = null; currentDropInfo = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
        });

        let editor_weak_for_drop = self.downgrade();
        let web_view_for_drop_eval = web_view.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            web_view_for_drop_eval.evaluate_javascript(
                "hideCaret(); currentDropRange = null;",
                None,
                None,
                None::<&gio::Cancellable>,
                |_| {},
            );
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(editor) = editor_weak_for_drop.upgrade() else {
                    return false;
                };
                let mut paths = Vec::new();
                if let Ok(file_list) = value.get::<gdk::FileList>() {
                    for file in file_list.files() {
                        if let Some(path) = file.path() {
                            if path.is_file() {
                                paths.push(path);
                            }
                        }
                    }
                } else if let Ok(file) = value.get::<gio::File>() {
                    if let Some(path) = file.path() {
                        if path.is_file() {
                            paths.push(path);
                        }
                    }
                }
                if !paths.is_empty() {
                    editor.dispatch_files_dropped(paths);
                    return true;
                }
                false
            }));
            res.unwrap_or(false)
        });
        web_view.add_controller(drop_target);

        let wv_key_controller = gtk::EventControllerKey::new();
        wv_key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let editor_weak_for_keys = self.downgrade();
        wv_key_controller.connect_key_pressed(move |_, keyval, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK)
                || state.contains(gdk::ModifierType::META_MASK);
            let alt = state.contains(gdk::ModifierType::ALT_MASK);
            let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
            let Some(editor) = editor_weak_for_keys.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if ctrl && !alt {
                match keyval {
                    gdk::Key::z | gdk::Key::Z => {
                        if shift {
                            editor.redo();
                        } else {
                            editor.undo();
                        }
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::y | gdk::Key::Y if !shift => {
                        editor.redo();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::b | gdk::Key::B if !shift => {
                        editor.toggle_bold();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::i | gdk::Key::I if !shift => {
                        editor.toggle_italic();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::u | gdk::Key::U if !shift => {
                        editor.toggle_underline();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::k | gdk::Key::K if !shift => {
                        editor.open_link_popover();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::x | gdk::Key::X if shift => {
                        editor.toggle_strikethrough();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::backslash if !shift => {
                        editor.clear_formatting();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::c | gdk::Key::C if shift => {
                        editor.copy_formatting();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::v | gdk::Key::V if shift => {
                        editor.paste_or_apply_formatting();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::_0 | gdk::Key::KP_0 if !shift => {
                        editor.set_block_format_index(0);
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::_1 | gdk::Key::KP_1 if !shift => {
                        editor.set_block_format_index(1);
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::_2 | gdk::Key::KP_2 if !shift => {
                        editor.set_block_format_index(2);
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::_3 | gdk::Key::KP_3 if !shift => {
                        editor.set_block_format_index(3);
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::_4 | gdk::Key::KP_4 if !shift => {
                        editor.set_block_format_index(4);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            } else if ctrl && alt && !shift {
                match keyval {
                    gdk::Key::c | gdk::Key::C => {
                        editor.copy_formatting();
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::v | gdk::Key::V => {
                        editor.apply_copied_formatting();
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            glib::Propagation::Proceed
        });
        web_view.add_controller(wv_key_controller);

        *self.imp().web_view.borrow_mut() = Some(web_view.clone());
        let initial_auto_fmt = AppSettings::try_new()
            .map(|s| s.composer_auto_format())
            .unwrap_or(true);
        self.set_auto_format_enabled(initial_auto_fmt);
        let editor_weak = self.downgrade();
        web_view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                if let Some(editor) = editor_weak.upgrade() {
                    editor.flush_pending_scripts();
                }
            }
        });
        web_view.load_html(&initial_html, None);

        let web_view_for_theme = web_view.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |sm| {
            if let Some(wv) = web_view_for_theme.upgrade() {
                let is_dark = sm.is_dark()
                    || AppSettings::try_new()
                        .map(|s| s.force_dark_mode())
                        .unwrap_or(false);
                let script = if is_dark {
                    "if (document.documentElement) { document.documentElement.classList.remove('meridian-light'); document.documentElement.classList.add('meridian-dark'); }"
                } else {
                    "if (document.documentElement) { document.documentElement.classList.remove('meridian-dark'); document.documentElement.classList.add('meridian-light'); }"
                };
                wv.evaluate_javascript(script, None, None, None::<&gio::Cancellable>, |_| {});
            }
        });

        let web_view_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&web_view)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["composer-editor-scrolled"])
            .build();

        self.imp()
            .web_view_scroll
            .replace(Some(web_view_scroll.clone()));

        let link_preview_label = gtk::Label::builder()
            .xalign(0.0)
            .single_line_mode(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .max_width_chars(90)
            .use_markup(true)
            .can_target(false)
            .focusable(false)
            .build();
        let link_preview_pill = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .can_target(false)
            .focusable(false)
            .visible(false)
            .css_classes(["link-preview-pill"])
            .build();
        link_preview_pill.append(&link_preview_label);

        let editor_overlay = gtk::Overlay::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&web_view_scroll)
            .build();
        editor_overlay.add_overlay(&link_preview_pill);
        editor_overlay.set_measure_overlay(&link_preview_pill, false);

        let editor_weak_for_sel = self.downgrade();
        user_content_manager.connect_local(
            "script-message-received::meridianSelectionState",
            false,
            move |values| {
                let Some(editor) = editor_weak_for_sel.upgrade() else {
                    return None;
                };
                let Some(js_val) = values
                    .get(1)
                    .and_then(|v| v.get::<javascriptcore6::Value>().ok())
                else {
                    return None;
                };
                let raw = js_val.to_str();
                editor.apply_selection_state_json(&raw);
                None
            },
        );

        if let Some(settings) = AppSettings::try_new() {
            let wv_weak = web_view.downgrade();
            let editor_weak_font = self.downgrade();
            let id_font = settings.connect_changed(Some("mail-font-family"), move |s, _| {
                let raw = s.string("mail-font-family").to_string();
                let safe = crate::views::mail_viewer::safe_css_font_family(&raw);
                if let Some(wv) = wv_weak.upgrade() {
                    if let Some(wk_settings) = webkit6::prelude::WebViewExt::settings(&wv) {
                        let desktop_font = crate::views::mail_viewer::detect_desktop_font_family();
                        let primary_font = primary_webkit_font_family(&raw, &desktop_font);
                        wk_settings.set_default_font_family(&primary_font);
                        wk_settings.set_sans_serif_font_family(&primary_font);
                    }
                }
                if let Some(editor) = editor_weak_font.upgrade() {
                    if let Some(dd) = editor.imp().dropdown_font_family.borrow().as_ref() {
                        let idx = font_family_index_for_css(&raw);
                        if dd.selected() != idx {
                            editor.imp().suppress_toolbar_sync.set(true);
                            dd.set_selected(idx);
                            editor.imp().suppress_toolbar_sync.set(false);
                        }
                    }
                    let mut updated_custom = false;
                    {
                        let mut styles = editor.imp().custom_styles.borrow_mut();
                        if let Some(p_style) = styles.get_mut("p") {
                            p_style.font_family = raw.clone();
                            save_custom_composer_styles(&styles);
                            updated_custom = true;
                        }
                    }
                    if updated_custom {
                        editor.apply_custom_styles_to_dom();
                    }
                    if let Ok(encoded) = serde_json::to_string(&safe) {
                        let js = format!(
                            "if (document.documentElement) document.documentElement.style.fontFamily = {encoded}; if (document.body) {{ document.body.style.fontFamily = {encoded}; if (!window.__meridianUserEdited) {{ document.body.querySelectorAll('p').forEach(p => {{ if (!p.closest('#_evolution_signature_, .evolution-signature')) p.style.removeProperty('font-family'); }}); }} }} if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();"
                        );
                        editor.evaluate_or_queue(js);
                    }
                }
            });
            let wv_weak_size = web_view.downgrade();
            let editor_weak_size = self.downgrade();
            let id_size = settings.connect_changed(Some("mail-font-size"), move |s, _| {
                let sz = s.int("mail-font-size").clamp(8, 48) as u32;
                if let Some(wv) = wv_weak_size.upgrade() {
                    if let Some(wk_settings) = webkit6::prelude::WebViewExt::settings(&wv) {
                        wk_settings.set_default_font_size(sz);
                    }
                }
                if let Some(editor) = editor_weak_size.upgrade() {
                    if let Some(dd) = editor.imp().dropdown_font_size.borrow().as_ref() {
                        let idx = font_size_index_for_px(sz);
                        if dd.selected() != idx {
                            editor.imp().suppress_toolbar_sync.set(true);
                            dd.set_selected(idx);
                            editor.imp().suppress_toolbar_sync.set(false);
                        }
                    }
                    let mut updated_custom = false;
                    {
                        let mut styles = editor.imp().custom_styles.borrow_mut();
                        if let Some(p_style) = styles.get_mut("p") {
                            p_style.font_size_px = sz;
                            save_custom_composer_styles(&styles);
                            updated_custom = true;
                        }
                    }
                    if updated_custom {
                        editor.apply_custom_styles_to_dom();
                    }
                    let js = format!(
                        "if (document.documentElement) document.documentElement.style.fontSize = '{sz}px'; if (document.body) {{ document.body.style.fontSize = '{sz}px'; if (!window.__meridianUserEdited) {{ document.body.querySelectorAll('p').forEach(p => {{ if (!p.closest('#_evolution_signature_, .evolution-signature')) p.style.removeProperty('font-size'); }}); }} }} if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();"
                    );
                    editor.evaluate_or_queue(js);
                }
            });
            self.imp().spelling_handler_ids.borrow_mut().push(id_font);
            self.imp().spelling_handler_ids.borrow_mut().push(id_size);
        }

        let pill_for_hover = link_preview_pill.clone();
        let label_for_hover = link_preview_label.clone();
        web_view.connect_mouse_target_changed(move |_, hit_test_result, _| {
            let formatted = if hit_test_result.context_is_link() {
                hit_test_result
                    .link_uri()
                    .and_then(|u| crate::views::mail_viewer::format_hovered_link_preview(&u))
            } else {
                None
            };
            if let Some((plain, markup)) = formatted {
                label_for_hover.set_markup(&markup);
                label_for_hover.set_tooltip_text(Some(&plain));
                pill_for_hover.set_visible(true);
            } else {
                pill_for_hover.set_visible(false);
                label_for_hover.set_text("");
                label_for_hover.set_tooltip_text(None);
            }
        });

        let motion_leave = gtk::EventControllerMotion::new();
        let pill_for_leave = link_preview_pill;
        let label_for_leave = link_preview_label;
        motion_leave.connect_leave(move |_| {
            pill_for_leave.set_visible(false);
            label_for_leave.set_text("");
            label_for_leave.set_tooltip_text(None);
        });
        web_view.add_controller(motion_leave);

        let click_dismiss_popovers = gtk::GestureClick::new();
        click_dismiss_popovers.set_propagation_phase(gtk::PropagationPhase::Capture);
        click_dismiss_popovers.set_button(0);
        let editor_for_click_dismiss = self.downgrade();
        click_dismiss_popovers.connect_pressed(move |_, _, _, _| {
            if let Some(editor) = editor_for_click_dismiss.upgrade() {
                editor.popdown_toolbar_popovers();
            }
        });
        web_view.add_controller(click_dismiss_popovers);

        vbox.append(&editor_overlay);
        vbox.set_parent(self);

        // Toolbar action wiring
        let editor_for_bold = self.downgrade();
        btn_bold.connect_clicked(move |_| {
            if let Some(editor) = editor_for_bold.upgrade() {
                editor.toggle_bold();
            }
        });

        let editor_for_italic = self.downgrade();
        btn_italic.connect_clicked(move |_| {
            if let Some(editor) = editor_for_italic.upgrade() {
                editor.toggle_italic();
            }
        });

        let editor_for_underline = self.downgrade();
        btn_underline.connect_clicked(move |_| {
            if let Some(editor) = editor_for_underline.upgrade() {
                editor.toggle_underline();
            }
        });

        let editor_for_strike = self.downgrade();
        btn_strikethrough.connect_clicked(move |_| {
            if let Some(editor) = editor_for_strike.upgrade() {
                editor.toggle_strikethrough();
            }
        });

        let editor_for_clear = self.downgrade();
        btn_clear.connect_clicked(move |_| {
            if let Some(editor) = editor_for_clear.upgrade() {
                editor.clear_formatting();
            }
        });

        let editor_for_format = self.downgrade();
        dropdown_format.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let Some(editor) = editor_for_format.upgrade() else {
                    return;
                };
                editor.sync_typography_popover_ui();
                if editor.imp().suppress_toolbar_sync.get() {
                    return;
                }
                let selected = dropdown.selected();
                let tag = match selected {
                    1 => "h1",
                    2 => "h2",
                    3 => "h3",
                    4 => "pre",
                    _ => "p",
                };
                let script = format!(
                    r#"(() => {{
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    const tag = '{tag}';
    if (tag === 'p' && typeof window.meridianUnwrapBlockquote === 'function') {{
        while (window.meridianUnwrapBlockquote()) {{}}
    }}
    const sel = window.getSelection();
    if (sel && sel.isCollapsed && sel.rangeCount > 0) {{
        let node = sel.anchorNode || sel.getRangeAt(0).startContainer;
        if (node && node.nodeType === 3) node = node.parentElement;
        if (node === document.body && document.body.firstElementChild) node = document.body.firstElementChild;
        const sig = document.getElementById('_evolution_signature_') || document.querySelector('.evolution-signature');
        const block = node && node.closest ? node.closest('p, div, h1, h2, h3, pre') : null;
        if (block && block !== sig && !(sig && sig.contains(block)) && block.parentNode) {{
            const nextEl = document.createElement(tag);
            while (block.firstChild) nextEl.appendChild(block.firstChild);
            if (isBlockEmpty(nextEl)) nextEl.innerHTML = '<br>';
            block.parentNode.replaceChild(nextEl, block);
            const nr = document.createRange();
            if (isBlockEmpty(nextEl)) {{
                nr.setStart(nextEl, 0);
                nr.collapse(true);
            }} else {{
                nr.selectNodeContents(nextEl);
                nr.collapse(false);
            }}
            sel.removeAllRanges();
            sel.addRange(nr);
        }} else {{
            document.execCommand('formatBlock', false, tag);
        }}
    }} else {{
        document.execCommand('formatBlock', false, tag);
        if (tag === 'p' && sel && !sel.isCollapsed) {{
            document.execCommand('removeFormat', false, null);
        }}
    }}
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();
    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();
}})()"#
                );
                web_view.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let editor_for_font = self.downgrade();
        dropdown_font_family.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let Some(editor) = editor_for_font.upgrade() else {
                    return;
                };
                editor.sync_typography_popover_ui();
                if editor.imp().suppress_toolbar_sync.get() {
                    return;
                };
                let idx = dropdown.selected() as usize;
                let css_family = COMPOSER_FONT_FAMILIES
                    .get(idx)
                    .map(|(_, css)| *css)
                    .unwrap_or("system-ui, sans-serif");
                let css_family = if idx == 0 {
                    AppSettings::try_new()
                        .map(|s| s.mail_font_family())
                        .unwrap_or_else(|| css_family.to_string())
                } else {
                    css_family.to_string()
                };
                let safe_family = crate::views::mail_viewer::safe_css_font_family(&css_family);
                let Ok(encoded) = serde_json::to_string(&safe_family) else {
                    return;
                };
                let script = format!(
                    r#"(() => {{
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    window.__meridianSuppressSelectionNotify = true;
    try {{
        const sel = window.getSelection();
        document.execCommand('styleWithCSS', false, true);
        if (sel && !sel.isCollapsed) {{
            document.execCommand('fontName', false, {encoded});
        }} else {{
            let node = sel && sel.rangeCount > 0 ? (sel.focusNode || sel.getRangeAt(0).startContainer) : null;
            if (node && node.nodeType === 3) node = node.parentElement;
            if (node === document.body && document.body && document.body.firstElementChild) {{
                node = document.body.firstElementChild;
            }}
            const block = node && node.closest ? node.closest('p, div, li, h1, h2, h3, pre, blockquote') : null;
            if (node && node.tagName === 'SPAN' && node.textContent === '\u200b') {{
                node.style.fontFamily = {encoded};
            }} else if (block && typeof isBlockEmpty === 'function' && isBlockEmpty(block)) {{
                block.style.fontFamily = {encoded};
                if (!window.__meridianUserEdited) {{
                    document.documentElement.style.fontFamily = {encoded};
                    document.body.style.fontFamily = {encoded};
                }}
            }} else if (sel && sel.rangeCount > 0) {{
                const range = sel.getRangeAt(0);
                const span = document.createElement('span');
                span.style.fontFamily = {encoded};
                span.textContent = '\u200b';
                range.insertNode(span);
                range.setStart(span.firstChild, 1);
                range.collapse(true);
                sel.removeAllRanges();
                sel.addRange(range);
            }} else {{
                document.body.style.fontFamily = {encoded};
            }}
        }}
    }} finally {{
        window.__meridianSuppressSelectionNotify = false;
    }}
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();
    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();
}})()"#
                );
                web_view.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let editor_for_size = self.downgrade();
        dropdown_font_size.connect_selected_notify(glib::clone!(
            #[weak]
            web_view,
            move |dropdown| {
                let Some(editor) = editor_for_size.upgrade() else {
                    return;
                };
                editor.sync_typography_popover_ui();
                if editor.imp().suppress_toolbar_sync.get() {
                    return;
                }
                let size = font_size_px_for_index(dropdown.selected());
                let script = if size == 0 {
                    "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('styleWithCSS', false, true); document.execCommand('fontSize', false, '3'); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();".to_string()
                } else {
                    format!(
                        r#"(() => {{
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    window.__meridianSuppressSelectionNotify = true;
    try {{
        const sel = window.getSelection();
        if (sel && !sel.isCollapsed) {{
            document.execCommand('styleWithCSS', false, false);
            document.execCommand('fontSize', false, '7');
            const markers = document.querySelectorAll('font[size="7"], span[style*="xxx-large"], [style*="-webkit-xxx-large"]');
            for (let i = 0; i < markers.length; i++) {{
                const el = markers[i];
                if (el.tagName === 'FONT') {{
                    const sp = document.createElement('span');
                    sp.style.fontSize = '{size}px';
                    while (el.firstChild) sp.appendChild(el.firstChild);
                    if (el.parentNode) el.parentNode.replaceChild(sp, el);
                }} else {{
                    el.removeAttribute('size');
                    el.style.fontSize = '{size}px';
                }}
            }}
        }} else {{
            let node = sel && sel.rangeCount > 0 ? (sel.focusNode || sel.getRangeAt(0).startContainer) : null;
            if (node && node.nodeType === 3) node = node.parentElement;
            if (node === document.body && document.body && document.body.firstElementChild) {{
                node = document.body.firstElementChild;
            }}
            const block = node && node.closest ? node.closest('p, div, li, h1, h2, h3, pre, blockquote') : null;
            if (node && node.tagName === 'SPAN' && node.textContent === '\u200b') {{
                node.style.fontSize = '{size}px';
            }} else if (block && typeof isBlockEmpty === 'function' && isBlockEmpty(block)) {{
                block.style.fontSize = '{size}px';
                const innerSpans = block.querySelectorAll('span[style*="font-size"]');
                for (let j = 0; j < innerSpans.length; j++) {{
                    innerSpans[j].style.fontSize = '{size}px';
                }}
                if (!window.__meridianUserEdited) {{
                    document.documentElement.style.fontSize = '{size}px';
                    document.body.style.fontSize = '{size}px';
                }}
            }} else if (sel && sel.rangeCount > 0) {{
                const range = sel.getRangeAt(0);
                const span = document.createElement('span');
                span.style.fontSize = '{size}px';
                span.textContent = '\u200b';
                range.insertNode(span);
                range.setStart(span.firstChild, 1);
                range.collapse(true);
                sel.removeAllRanges();
                sel.addRange(range);
            }} else {{
                document.body.style.fontSize = '{size}px';
            }}
        }}
    }} finally {{
        window.__meridianSuppressSelectionNotify = false;
    }}
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();
    if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();
}})()"#,
                        size = size
                    )
                };
                web_view.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('insertUnorderedList', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_ordered_list.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('insertOrderedList', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState();",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        btn_blockquote.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "if (typeof window.meridianToggleBlockquote === 'function') { window.meridianToggleBlockquote(); } else { if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (!(typeof window.meridianUnwrapBlockquote === 'function' && window.meridianUnwrapBlockquote())) { document.execCommand('formatBlock', false, 'blockquote'); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged(); if (typeof window.notifyMeridianSelectionState === 'function') window.notifyMeridianSelectionState(); } }",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let entry_url_weak_for_open = entry_url.downgrade();
        let entry_label_weak_for_open = entry_label.downgrade();
        let web_view_for_open = web_view.clone();
        link_popover.connect_notify_local(Some("visible"), move |popover, _| {
            if !popover.is_visible() {
                return;
            }
            let Some(entry_url) = entry_url_weak_for_open.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak_for_open.upgrade() else {
                return;
            };
            let entry_u = entry_url.clone();
            let entry_l = entry_label.clone();
            web_view_for_open.evaluate_javascript(
                r#"(() => {
    let sel = window.getSelection();
    let text = sel ? sel.toString().trim() : '';
    let href = '';
    if (sel && sel.rangeCount > 0) {
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        let a = node ? node.closest('a') : null;
        if (a) {
            href = a.getAttribute('href') || a.href || '';
            if (!text) text = a.textContent ? a.textContent.trim() : '';
        }
    }
    return JSON.stringify({ text, href });
})()"#,
                None,
                None,
                None::<&gio::Cancellable>,
                move |result| {
                    if let Ok(val) = result {
                        let json_str = val.to_str();
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let href = parsed.get("href").and_then(|v| v.as_str()).unwrap_or("");
                            entry_u.set_text(href);
                            entry_l.set_text(text);
                            if href.is_empty() {
                                entry_u.grab_focus();
                            } else {
                                entry_l.grab_focus();
                            }
                        }
                    }
                },
            );
        });

        let web_view_for_link = web_view.clone();
        let link_popover_weak = link_popover.downgrade();
        let entry_url_weak = entry_url.downgrade();
        let entry_label_weak = entry_label.downgrade();
        btn_insert_link_confirm.connect_clicked(move |_| {
            let Some(entry_url) = entry_url_weak.upgrade() else {
                return;
            };
            let Some(entry_label) = entry_label_weak.upgrade() else {
                return;
            };
            let mut url = entry_url.text().trim().to_string();
            let label = entry_label.text().trim().to_string();
            if !url.is_empty() {
                if !url.contains("://") && !url.starts_with("mailto:") && !url.starts_with('#') {
                    if url.contains('@') && !url.contains('/') {
                        url = format!("mailto:{url}");
                    } else {
                        url = format!("https://{url}");
                    }
                }

                let url_json = serde_json::to_string(&url).unwrap_or_else(|_| format!("\"{}\"", url));
                let label_json = serde_json::to_string(&label).unwrap_or_else(|_| format!("\"{}\"", label));

                let js = format!(
                    r#"(() => {{
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    const url = {url_json};
    const label = {label_json};
    const sel = window.getSelection();
    let existingA = null;
    if (sel && sel.rangeCount > 0) {{
        let node = sel.anchorNode;
        if (node && node.nodeType === 3) node = node.parentElement;
        existingA = node ? node.closest('a') : null;
    }}
    if (existingA) {{
        existingA.href = url;
        if (label) existingA.textContent = label;
    }} else if (label) {{
        const a = document.createElement('a');
        a.href = url;
        a.textContent = label;
        if (sel && sel.rangeCount > 0) {{
            const range = sel.getRangeAt(0);
            range.deleteContents();
            range.insertNode(a);
            range.setStartAfter(a);
            range.setEndAfter(a);
            sel.removeAllRanges();
            sel.addRange(range);
        }} else {{
            document.body.appendChild(a);
        }}
    }} else {{
        if (sel && !sel.isCollapsed) {{
            document.execCommand('createLink', false, url);
        }} else {{
            const a = document.createElement('a');
            a.href = url;
            a.textContent = url;
            if (sel && sel.rangeCount > 0) {{
                const range = sel.getRangeAt(0);
                range.deleteContents();
                range.insertNode(a);
                range.setStartAfter(a);
                range.setEndAfter(a);
                sel.removeAllRanges();
                sel.addRange(range);
            }} else {{
                document.body.appendChild(a);
            }}
        }}
    }}
    if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true);
    if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();
}})();"#
                );

                web_view_for_link.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
                entry_url.set_text("");
                entry_label.set_text("");
            }
            if let Some(popover) = link_popover_weak.upgrade() {
                popover.popdown();
            }
        });

        btn_rule.connect_clicked(glib::clone!(
            #[weak]
            web_view,
            move |_| {
                web_view.evaluate_javascript(
                    "if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); document.execCommand('insertHorizontalRule', false, null); if (typeof window.meridianRecordUndoSnapshot === 'function') window.meridianRecordUndoSnapshot(true); if (typeof window.notifyMeridianContentChanged === 'function') window.notifyMeridianContentChanged();",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        ));

        let editor_for_image = self.downgrade();
        btn_image.connect_clicked(move |_| {
            let Some(editor) = editor_for_image.upgrade() else {
                return;
            };
            let parent = editor.root().and_downcast::<gtk::Window>();
            let file_dialog = gtk::FileDialog::builder()
                .title(gettext("Select image to insert"))
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext("Images (*.png, *.jpg, *.jpeg, *.gif, *.svg, *.webp)")));
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("image/gif");
            filter.add_mime_type("image/svg+xml");
            filter.add_mime_type("image/webp");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            file_dialog.set_filters(Some(&filters));

            let editor_weak = editor_for_image.clone();
            file_dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        if let Some(editor) = editor_weak.upgrade() {
                            editor.insert_image_from_path(&path);
                        }
                    }
                }
            });
        });

        let editor_for_undo = self.downgrade();
        btn_undo.connect_clicked(move |_| {
            if let Some(editor) = editor_for_undo.upgrade() {
                editor.undo();
            }
        });

        let editor_for_redo = self.downgrade();
        btn_redo.connect_clicked(move |_| {
            if let Some(editor) = editor_for_redo.upgrade() {
                editor.redo();
            }
        });
    }
}

impl Default for HtmlEditor {
    fn default() -> Self {
        Self::new()
    }
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(
            TABLE[(((first & 0x03) << 4) | second.map_or(0, |value| value >> 4)) as usize] as char,
        );
        encoded.push(second.map_or('=', |value| {
            TABLE[(((value & 0x0f) << 2) | third.map_or(0, |third| third >> 6)) as usize] as char
        }));
        encoded.push(third.map_or('=', |value| TABLE[(value & 0x3f) as usize] as char));
    }

    encoded
}

#[derive(Debug, serde::Deserialize)]
struct ImageMenuPayload {
    id: String,
    x: i32,
    y: i32,
    #[allow(dead_code)]
    img_x: i32,
    #[allow(dead_code)]
    img_y: i32,
    #[allow(dead_code)]
    img_w: i32,
    #[allow(dead_code)]
    img_h: i32,
    align: String,
    size: String,
}

#[derive(Debug, serde::Deserialize)]
struct ConvertedImageData {
    src: String,
    #[serde(default)]
    alt: String,
}

pub fn parse_image_data_uri_or_path(src: &str, alt: &str) -> Option<(Vec<u8>, String, String)> {
    if let Some(rest) = src.strip_prefix("data:") {
        let mut parts = rest.splitn(2, ',');
        let header = parts.next()?;
        let body = parts.next()?;
        let mime = header.split(';').next().unwrap_or("image/png").trim();
        let is_base64 = header.contains(";base64");
        let bytes = if is_base64 {
            glib::base64_decode(body.trim())
        } else {
            body.as_bytes().to_vec()
        };
        if bytes.is_empty() {
            return None;
        }
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" | "image/jpg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            _ => "png",
        };
        let alt_clean = alt.trim();
        let filename = if !alt_clean.is_empty() && alt_clean.contains('.') {
            alt_clean.to_string()
        } else if !alt_clean.is_empty() {
            format!("{alt_clean}.{ext}")
        } else {
            format!("image_{}.{ext}", &glib::uuid_string_random()[..6])
        };
        Some((bytes, mime.to_string(), filename))
    } else if let Some(_path_str) = src.strip_prefix("file://") {
        let file = gio::File::for_uri(src);
        let path = file.path()?;
        let bytes = std::fs::read(&path).ok()?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => "application/octet-stream",
        };
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png")
            .to_string();
        Some((bytes, mime.to_string(), filename))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_image_data_uri_png() {
        let dummy_png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let data_uri = format!("data:image/png;base64,{dummy_png_b64}");
        let result = parse_image_data_uri_or_path(&data_uri, "my_screenshot.png");
        assert!(result.is_some());
        let (bytes, mime, filename) = result.unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(filename, "my_screenshot.png");
        assert!(!bytes.is_empty());
        assert_eq!(bytes[0], 0x89);
        assert_eq!(bytes[1], 0x50); // 'P'
        assert_eq!(bytes[2], 0x4e); // 'N'
        assert_eq!(bytes[3], 0x47); // 'G'
    }

    #[test]
    fn test_parse_image_data_uri_custom_alt() {
        let dummy_b64 = "dGVzdA=="; // "test"
        let data_uri = format!("data:image/jpeg;base64,{dummy_b64}");
        let result = parse_image_data_uri_or_path(&data_uri, "photo");
        assert!(result.is_some());
        let (bytes, mime, filename) = result.unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(filename, "photo.jpg");
        assert_eq!(bytes, b"test");
    }

    #[test]
    fn test_parse_image_data_uri_invalid() {
        assert!(parse_image_data_uri_or_path("invalid:not-a-data-uri", "").is_none());
        assert!(parse_image_data_uri_or_path("data:image/png;base64,", "").is_none());
    }

    #[test]
    fn test_image_menu_payload_deserialize() {
        let json = r#"{"id":"__meridian_img_123","x":150,"y":280,"img_x":100,"img_y":200,"img_w":400,"img_h":300,"align":"center","size":"medium"}"#;
        let payload: Result<ImageMenuPayload, _> = serde_json::from_str(json);
        assert!(payload.is_ok());
        let p = payload.unwrap();
        assert_eq!(p.id, "__meridian_img_123");
        assert_eq!(p.x, 150);
        assert_eq!(p.y, 280);
        assert_eq!(p.align, "center");
        assert_eq!(p.size, "medium");
    }

    #[test]
    fn test_image_menu_payload_quoted_deserialize() {
        let quoted_json = r#""{\"id\":\"__meridian_img_999\",\"x\":200,\"y\":350,\"img_x\":180,\"img_y\":300,\"img_w\":500,\"img_h\":400,\"align\":\"left\",\"size\":\"large\"}""#;
        let unquoted: String = serde_json::from_str(quoted_json).unwrap_or_else(|_| quoted_json.to_string());
        let payload: Result<ImageMenuPayload, _> = serde_json::from_str::<ImageMenuPayload>(&unquoted)
            .or_else(|_| serde_json::from_str::<ImageMenuPayload>(quoted_json));
        assert!(payload.is_ok());
        let p = payload.unwrap();
        assert_eq!(p.id, "__meridian_img_999");
        assert_eq!(p.x, 200);
        assert_eq!(p.y, 350);
        assert_eq!(p.align, "left");
        assert_eq!(p.size, "large");
    }

    #[test]
    fn test_prepare_image_data_uri() {
        let tmp = std::env::temp_dir().join("meridian_test_inline_prep.png");
        let png_bytes = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92,
            0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let _ = std::fs::write(&tmp, &png_bytes);
        let res = HtmlEditor::prepare_image_data_uri(&tmp);
        assert!(res.is_some());
        let (data_uri, mime) = res.unwrap();
        assert_eq!(mime, "image/png");
        assert!(data_uri.starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_converted_image_data_deserialize() {
        let raw_json = r#"{"src":"data:image/png;base64,dGVzdA==","alt":"test.png"}"#;
        let data: Result<ConvertedImageData, _> = serde_json::from_str(raw_json);
        assert!(data.is_ok());
        let d = data.unwrap();
        assert_eq!(d.src, "data:image/png;base64,dGVzdA==");
        assert_eq!(d.alt, "test.png");

        let quoted_json = r#""{\"src\":\"data:image/png;base64,dGVzdA==\",\"alt\":\"test.png\"}""#;
        let unquoted: String = serde_json::from_str(quoted_json).unwrap_or_else(|_| quoted_json.to_string());
        let data2: Result<ConvertedImageData, _> = serde_json::from_str(&unquoted);
        assert!(data2.is_ok());
        let d2 = data2.unwrap();
        assert_eq!(d2.src, "data:image/png;base64,dGVzdA==");
        assert_eq!(d2.alt, "test.png");
    }

    #[test]
    fn test_is_supported_image() {
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.png")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.JPG")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("photo.jpeg")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("animation.gif")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("vector.svg")));
        assert!(HtmlEditor::is_supported_image(std::path::Path::new("modern.webp")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("doc.pdf")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("archive.zip")));
        assert!(!HtmlEditor::is_supported_image(std::path::Path::new("text.txt")));
    }

    #[test]
    fn test_toolbar_dropdowns_have_fixed_width_and_do_not_expand() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        let dd_format = editor
            .imp()
            .dropdown_format
            .borrow()
            .clone()
            .expect("dropdown_format initialized");
        let dd_font = editor
            .imp()
            .dropdown_font_family
            .borrow()
            .clone()
            .expect("dropdown_font_family initialized");
        let dd_size = editor
            .imp()
            .dropdown_font_size
            .borrow()
            .clone()
            .expect("dropdown_font_size initialized");

        assert_eq!(dd_format.width_request(), DROPDOWN_FORMAT_WIDTH);
        assert_eq!(dd_font.width_request(), DROPDOWN_FONT_FAMILY_WIDTH);
        assert_eq!(dd_size.width_request(), DROPDOWN_FONT_SIZE_WIDTH);
        assert!(!dd_format.hexpands());
        assert!(!dd_font.hexpands());
        assert!(!dd_size.hexpands());
        assert!(dd_format.factory().is_some());
        assert!(dd_font.factory().is_some());
        assert!(dd_size.factory().is_some());
    }

    #[test]
    fn test_apply_selection_state_updates_top_toolbar_and_dropdowns() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        editor.imp().pending_scripts.borrow_mut().clear();

        let state_json = r#"{
            "has_selection": true,
            "bold": true,
            "italic": false,
            "underline": true,
            "strikethrough": false,
            "ul": true,
            "ol": false,
            "blockquote": true,
            "block_tag": "h2",
            "font_family": "Georgia, serif",
            "font_size": 18
        }"#;
        editor.apply_selection_state_json(state_json);

        let btn_bold = editor.imp().btn_bold.borrow().clone().unwrap();
        let btn_italic = editor.imp().btn_italic.borrow().clone().unwrap();
        let btn_underline = editor.imp().btn_underline.borrow().clone().unwrap();
        let btn_list = editor.imp().btn_list.borrow().clone().unwrap();
        let btn_blockquote = editor.imp().btn_blockquote.borrow().clone().unwrap();

        assert!(btn_bold.has_css_class("suggested-action"));
        assert!(!btn_italic.has_css_class("suggested-action"));
        assert!(btn_underline.has_css_class("suggested-action"));
        assert!(btn_list.has_css_class("suggested-action"));
        assert!(btn_blockquote.has_css_class("suggested-action"));

        let dd_format = editor.imp().dropdown_format.borrow().clone().unwrap();
        let dd_font = editor.imp().dropdown_font_family.borrow().clone().unwrap();
        let dd_size = editor.imp().dropdown_font_size.borrow().clone().unwrap();

        assert_eq!(dd_format.selected(), 2); // Heading 2
        assert_eq!(dd_font.selected(), font_family_index_for_css("Georgia, serif"));
        assert_eq!(dd_size.selected(), font_size_index_for_px(18)); // 18 px
        assert_eq!(font_size_px_for_index(font_size_index_for_px(11)), 11); // 11 px available!

        // Updating selection state must not queue formatting commands back into the WebView
        assert!(editor.imp().pending_scripts.borrow().is_empty());
    }

    #[test]
    fn test_undo_and_redo_queue_meridian_history_commands() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        editor.imp().pending_scripts.borrow_mut().clear();

        editor.undo();
        editor.redo();

        let scripts = editor.imp().pending_scripts.borrow().clone();
        assert_eq!(scripts.len(), 2);
        assert!(scripts[0].contains("window.meridianUndo"));
        assert!(scripts[1].contains("window.meridianRedo"));
    }

    #[test]
    fn test_compact_typography_color_and_align_popovers_in_toolbar() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        let toolbar = editor
            .imp()
            .format_toolbar
            .borrow()
            .clone()
            .expect("format_toolbar initialized");
        let btn_typography = editor
            .imp()
            .btn_typography
            .borrow()
            .clone()
            .expect("btn_typography initialized");
        let btn_color = editor
            .imp()
            .btn_color
            .borrow()
            .clone()
            .expect("btn_color initialized");
        let btn_align = editor
            .imp()
            .btn_align
            .borrow()
            .clone()
            .expect("btn_align initialized");

        assert!(btn_typography.popover().is_some());
        assert!(btn_color.popover().is_some());
        assert!(btn_align.popover().is_some());

        // Minimalist productivity layout: Style, Font, and Size dropdowns are grouped cleanly on the main toolbar
        let dd_format = editor.imp().dropdown_format.borrow().clone().unwrap();
        let dd_font = editor.imp().dropdown_font_family.borrow().clone().unwrap();
        let dd_size = editor.imp().dropdown_font_size.borrow().clone().unwrap();
        let btn_blockquote = editor.imp().btn_blockquote.borrow().clone().unwrap();
        let btn_link = editor.imp().btn_link.borrow().clone().unwrap();
        let btn_table = editor.imp().btn_table.borrow().clone().unwrap();
        let btn_rule = toolbar
            .last_child()
            .and_then(|w| w.downcast::<gtk::Button>().ok())
            .expect("last toolbar widget is btn_rule");
        assert_eq!(dd_format.parent().as_ref(), Some(toolbar.upcast_ref()));
        assert_eq!(dd_font.parent().as_ref(), Some(toolbar.upcast_ref()));
        assert_eq!(dd_size.parent().as_ref(), Some(toolbar.upcast_ref()));
        assert_eq!(btn_table.parent().as_ref(), Some(toolbar.upcast_ref()));
        assert!(btn_table.popover().is_some());
        assert!(btn_typography.parent().is_none());
        assert_eq!(
            btn_blockquote.icon_name().as_deref(),
            Some("meridian-format-quote-symbolic")
        );
        assert_eq!(
            btn_link.icon_name().as_deref(),
            Some("meridian-insert-link-symbolic")
        );
        assert_eq!(
            btn_table.icon_name().as_deref(),
            Some("meridian-insert-table-symbolic")
        );
        assert_eq!(
            btn_rule.icon_name().as_deref(),
            Some("meridian-horizontal-rule-symbolic")
        );
    }

    #[test]
    fn test_copy_and_apply_formatting_and_paste_match_style_queue_commands() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let editor = HtmlEditor::new();
        editor.imp().pending_scripts.borrow_mut().clear();

        editor.copy_formatting();
        editor.apply_copied_formatting();
        editor.insert_plain_text_matching_style("Hello Clean Paste");
        editor.set_text_color("#3584e4");
        editor.set_highlight_color("rgba(249, 240, 107, 0.45)");
        editor.set_text_alignment("justifyCenter");
        editor.set_text_alignment("indent");
        editor.set_text_alignment("outdent");
        editor.insert_table(3, 4);
        editor.modify_table("addRow");

        let scripts = editor.imp().pending_scripts.borrow().clone();
        assert_eq!(scripts.len(), 10);
        assert!(scripts[0].contains("window.meridianCopyStyle"));
        assert!(scripts[1].contains("window.meridianApplyCopiedStyle"));
        assert!(scripts[2].contains("window.meridianInsertPlainTextMatchStyle"));
        assert!(scripts[2].contains("Hello Clean Paste"));
        assert!(scripts[3].contains("foreColor") && scripts[3].contains("#3584e4"));
        assert!(scripts[4].contains("hiliteColor"));
        assert!(scripts[5].contains("justifyCenter"));
        assert!(scripts[6].contains("window.meridianIndent"));
        assert!(scripts[7].contains("window.meridianOutdent"));
        assert!(scripts[8].contains("window.meridianInsertTable(3, 4)"));
        assert!(scripts[9].contains("window.meridianModifyTable('addRow')"));
    }

    #[test]
    fn test_font_sizes_include_11px_and_custom_styles_can_be_saved_and_reset() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        assert!(COMPOSER_FONT_SIZES.contains(&11));
        assert!(COMPOSER_FONT_SIZES.windows(2).all(|w| w[0] < w[1]));
        for (expected_idx, &px) in COMPOSER_FONT_SIZES.iter().enumerate() {
            let idx = font_size_index_for_px(px);
            assert_eq!(idx, expected_idx as u32);
            assert_eq!(font_size_px_for_index(idx), px);
        }

        let editor = HtmlEditor::new();
        editor.imp().pending_scripts.borrow_mut().clear();

        // Select Heading 1, Inter font, 20 px, bold
        if let Some(dd) = editor.imp().dropdown_format.borrow().as_ref() {
            editor.imp().suppress_toolbar_sync.set(true);
            dd.set_selected(1); // h1
            editor.imp().suppress_toolbar_sync.set(false);
        }
        if let Some(dd) = editor.imp().dropdown_font_family.borrow().as_ref() {
            editor.imp().suppress_toolbar_sync.set(true);
            dd.set_selected(font_family_index_for_css("Inter, sans-serif"));
            editor.imp().suppress_toolbar_sync.set(false);
        }
        if let Some(dd) = editor.imp().dropdown_font_size.borrow().as_ref() {
            editor.imp().suppress_toolbar_sync.set(true);
            dd.set_selected(font_size_index_for_px(20));
            editor.imp().suppress_toolbar_sync.set(false);
        }
        if let Some(btn_bold) = editor.imp().btn_bold.borrow().as_ref() {
            btn_bold.add_css_class("suggested-action");
        }

        editor.save_current_style_preset();
        {
            let styles = editor.imp().custom_styles.borrow();
            let h1 = styles.get("h1").expect("h1 custom style saved");
            assert_eq!(h1.font_size_px, 20);
            assert_eq!(h1.bold, Some(true));
            let css = build_custom_styles_css(&styles);
            assert!(css.contains("h1 {"));
            assert!(css.contains("font-size: 20px;"));
        }

        editor.reset_current_style_preset();
        assert!(!editor.imp().custom_styles.borrow().contains_key("h1"));

        editor.set_auto_format_enabled(false);
        assert!(!editor.auto_format_enabled());
        assert!(editor
            .imp()
            .pending_scripts
            .borrow()
            .iter()
            .any(|s| s.contains("__meridianAutoFormatEnabled = false")));

        editor.set_block_format_index(2);
        assert_eq!(
            editor
                .imp()
                .dropdown_format
                .borrow()
                .as_ref()
                .unwrap()
                .selected(),
            2
        );

        for (expected_idx, (_, css_family)) in COMPOSER_FONT_FAMILIES.iter().enumerate() {
            let safe_css = crate::views::mail_viewer::safe_css_font_family(css_family);
            assert_eq!(
                font_family_index_for_css(&safe_css),
                expected_idx as u32,
                "Font family '{}' (safe '{}') must round-trip to index {}",
                css_family,
                safe_css,
                expected_idx
            );
        }
    }
}

