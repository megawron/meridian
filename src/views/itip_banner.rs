use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// ItipBanner — Interactive GNOME Evolution-parity iTIP/iCalendar invitation banner.
//
// Positioned between message headers and the email body (matching Evolution's
// itip-view.c), this banner displays meeting details and interactive actions:
// Accept, Tentative, Decline, Cancel, Calendar target selection, and
// Open-in-Calendar navigation.

use adw::prelude::*;
use chrono::{Datelike, NaiveDate};
use gtk::gio;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
#[cfg(target_os = "linux")]
use webkit6::prelude::*;

use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::itip_service::{
    find_user_status, format_recurrence_summary, update_attendee_partstat,
    ItipInvitation, ItipMethod, ItipReplyStatus,
};
use crate::services::settings_service::AppSettings;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CalendarPickerItem {
    pub uid: String,
    pub group_name: String,
    pub clean_name: String,
    pub picker_label: String,
}

#[derive(Clone)]
struct PlacedTimelineWidget {
    widget: gtk::Label,
    x: f64,
    col_idx: usize,
    total_cols: usize,
    start_min: u32,
    end_min: u32,
}

static NEXT_BANNER_ZOOM_ID: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(1);

#[derive(Clone)]
pub struct ItipBanner {
    revealer: gtk::Revealer,
    banner_box: gtk::Box,
    header_row: gtk::Box,
    details_revealer: gtk::Revealer,
    details_scrolled: gtk::ScrolledWindow,
    details_zoom: Rc<Cell<f64>>,
    details_zoom_class: String,
    details_zoom_provider: gtk::CssProvider,
    pointer_in_timeline: Rc<Cell<bool>>,
    cal_select_box: gtk::Box,
    footer_box: gtk::Box,
    is_compact: Rc<Cell<bool>>,
    collapse_guard_until: Rc<Cell<i64>>,
    full_header_prompt: Rc<RefCell<String>>,
    icon: gtk::Image,
    lbl_header_prompt: gtk::Label,
    lbl_cancel_inline: gtk::Label,
    lbl_title: gtk::Label,
    lbl_method: gtk::Label,
    lbl_status_badge: gtk::Label,
    lbl_time: gtk::Label,
    conflict_row: gtk::Box,
    conflict_icon: gtk::Image,
    lbl_conflict: gtk::Label,
    timeline_side_box: gtk::Box,
    lbl_timeline_date: gtk::Label,
    lbl_timeline_badge: gtk::Label,
    timeline_all_day_box: gtk::Box,
    timeline_hours_box: gtk::Box,
    timeline_hour_rows: Rc<RefCell<Vec<(gtk::Box, gtk::Box)>>>,
    timeline_hour_height: Rc<Cell<f64>>,
    timeline_placed_widgets: Rc<RefCell<Vec<PlacedTimelineWidget>>>,
    zoom_anchor_hour: Rc<Cell<Option<f64>>>,
    zoom_anchor_cleanup: Rc<RefCell<Option<glib::SourceId>>>,
    timeline_fixed: gtk::Fixed,
    timeline_scrolled: gtk::ScrolledWindow,
    timeline_target_scroll_y: Rc<Cell<f64>>,
    last_timeline_state: Rc<
        RefCell<
            Option<(
                crate::services::calendar_store_service::CalendarEventRecord,
                crate::services::calendar_store_service::CalendarSnapshot,
                bool,
                Vec<String>,
            )>,
        >,
    >,
    lbl_location: gtk::Label,
    location_map_popover: gtk::Popover,
    location_map_lbl: gtk::Label,
    location_map_open_btn: gtk::Button,
    current_location_text: Rc<RefCell<String>>,
    current_location_geo: Rc<Cell<Option<(f64, f64)>>>,
    lbl_organizer: gtk::Label,
    lbl_attendees: gtk::Label,
    lbl_recurrence: gtk::Label,
    lbl_description: gtk::Label,
    exp_description: gtk::Expander,
    sender_message_box: gtk::Box,
    lbl_sender_message: gtk::Label,
    calendar_status_box: gtk::Box,
    calendar_status_icon: gtk::Image,
    lbl_calendar_status: gtk::Label,
    chk_send_reply: gtk::CheckButton,
    comment_view: gtk::TextView,
    chk_show_free: gtk::CheckButton,
    chk_preserve_reminder: gtk::CheckButton,
    location_row: gtk::Box,
    recurrence_row: gtk::Box,
    organizer_row: gtk::Box,
    attendees_row: gtk::Box,
    calendar_dropdown: gtk::DropDown,
    calendar_sources: Rc<RefCell<Vec<(String, String)>>>,
    calendar_picker_items: Rc<RefCell<Vec<CalendarPickerItem>>>,
    calendar_source_colors: Rc<RefCell<std::collections::HashMap<String, String>>>,
    secondary_bar_box: gtk::Box,
    btn_target_calendar: gtk::MenuButton,
    lbl_calendar_dot: gtk::Box,
    lbl_calendar_chip_name: gtk::Label,
    calendar_chip_popover: gtk::Popover,
    calendar_chip_list_box: gtk::Box,
    sep_footer_inner: gtk::Separator,
    user_selected_calendar: Rc<Cell<bool>>,
    last_user_emails: Rc<RefCell<Vec<String>>>,
    btn_accept: gtk::Button,
    btn_tentative: gtk::Button,
    btn_decline: gtk::Button,
    btn_open_calendar: gtk::Button,
    btn_remove: gtk::Button,
    btn_update: gtk::Button,
    btn_import: gtk::Button,
    btn_import_as_event: gtk::Button,
    lbl_import_hint: gtk::Label,
    action_box_request: gtk::Box,
    action_box_cancel: gtk::Box,
    action_box_reply: gtk::Box,
    action_box_publish: gtk::Box,
    comment_revealer: gtk::Revealer,
    btn_toggle_comment: gtk::ToggleButton,
    btn_more_options: gtk::MenuButton,
    current_invitation: Rc<RefCell<Option<ItipInvitation>>>,
    on_calendar_navigate: Rc<RefCell<Option<Box<dyn Fn(NaiveDate) + 'static>>>>,
    on_action_toast: Rc<RefCell<Option<Box<dyn Fn(String) + 'static>>>>,
    on_reply_draft_saved: Rc<RefCell<Option<Box<dyn Fn(&ItipInvitation, ItipReplyStatus, Option<String>, bool) + 'static>>>>,
    on_scroll_offset: Rc<RefCell<Option<Box<dyn Fn(f64) + 'static>>>>,
}

impl std::fmt::Debug for ItipBanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItipBanner")
            .field("revealed", &self.revealer.reveals_child())
            .finish()
    }
}

impl ItipBanner {
    pub fn new() -> Self {
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::None)
            .reveal_child(false)
            .visible(false)
            .hexpand(true)
            .build();

        let banner_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .css_classes(vec!["itip-banner"])
            .hexpand(true)
            .vexpand(true)
            .margin_start(0)
            .margin_end(0)
            .margin_top(0)
            .margin_bottom(0)
            .build();

        // ── Top Header Prompt ─────────────────────────────────────────────────
        let header_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .margin_start(8)
            .margin_end(8)
            .margin_top(10)
            .margin_bottom(10)
            .build();

        let icon = gtk::Image::builder()
            .icon_name("system-users-symbolic")
            .pixel_size(24)
            .css_classes(vec!["accent"])
            .valign(gtk::Align::Center)
            .build();
        header_row.append(&icon);

        let lbl_header_prompt = gtk::Label::builder()
            .label(&gettext("Meeting Invitation"))
            .css_classes(vec!["heading", "itip-header-prompt"])
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .build();
        header_row.append(&lbl_header_prompt);

        let lbl_method = gtk::Label::builder()
            .label(&gettext("Meeting Invitation"))
            .visible(false)
            .build();

        let lbl_status_badge = gtk::Label::builder()
            .label(&gettext("Response Pending"))
            .css_classes(vec!["itip-status-badge", "pending"])
            .valign(gtk::Align::Center)
            .single_line_mode(true)
            .build();
        header_row.append(&lbl_status_badge);
        banner_box.append(&header_row);

        let sep_top = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep_top.set_hexpand(true);

        // ── Main 2-Column Content ─────────────────────────────────────────────
        let zoom_id = NEXT_BANNER_ZOOM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let details_zoom_class = format!("itip-details-zoom-{zoom_id}");
        let details_zoom_provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &details_zoom_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }

        let details_vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .hexpand(true)
            .valign(gtk::Align::Fill)
            .css_classes(vec![details_zoom_class.as_str()])
            .build();

        let details_info_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .margin_start(20)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(10)
            .build();

        // 1. Event Title (hidden in UI because the Mail Header directly above already shows the subject prominently; kept for state/test compatibility)
        let lbl_title = gtk::Label::builder()
            .label(&gettext("Event"))
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .visible(false)
            .build();
        details_info_box.append(&lbl_title);

        // 2. Metadata Rows (Date & Time is the primary entry in the left column)
        let time_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .build();
        let time_icon = gtk::Image::builder()
            .icon_name("alarm-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_time = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .css_classes(vec!["heading"])
            .build();
        time_row.append(&time_icon);
        time_row.append(&lbl_time);
        details_info_box.append(&time_row);

        let conflict_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let conflict_icon = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_conflict = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        conflict_row.append(&conflict_icon);
        conflict_row.append(&lbl_conflict);
        details_info_box.append(&conflict_row);

        let recurrence_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let rec_icon = gtk::Image::builder()
            .icon_name("media-playlist-repeat-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_recurrence = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        recurrence_row.append(&rec_icon);
        recurrence_row.append(&lbl_recurrence);
        details_info_box.append(&recurrence_row);

        ensure_itip_base_css();

        let location_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Start)
            .hexpand(false)
            .visible(false)
            .css_classes(vec!["itip-location-row", "itip-location-interactive"])
            .build();
        let loc_icon = gtk::Image::builder()
            .icon_name("find-location-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label", "itip-location-icon"])
            .build();
        let lbl_location = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(false)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(vec!["body", "itip-location-label"])
            .build();
        location_row.append(&loc_icon);
        location_row.append(&lbl_location);
        details_info_box.append(&location_row);

        let current_location_text = Rc::new(RefCell::new(String::new()));
        let current_location_geo: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

        // Minimalist Location Hover Map Popover
        let loc_pop_vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(268)
            .build();
        let location_map_frame = gtk::Frame::builder()
            .height_request(152)
            .hexpand(true)
            .css_classes(vec!["itip-location-map-frame"])
            .build();
        location_map_frame.set_overflow(gtk::Overflow::Hidden);
        loc_pop_vbox.append(&location_map_frame);

        let loc_pop_footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .margin_start(2)
            .margin_end(2)
            .build();
        let location_map_lbl = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        let location_map_copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(&gettext("Copy location"))
            .css_classes(vec!["flat", "circular", "itip-map-action-btn"])
            .valign(gtk::Align::Center)
            .build();
        let location_map_open_btn = gtk::Button::builder()
            .icon_name("web-browser-symbolic")
            .tooltip_text(&gettext("Open in Map"))
            .css_classes(vec!["flat", "circular", "itip-map-action-btn"])
            .valign(gtk::Align::Center)
            .build();
        loc_pop_footer.append(&location_map_lbl);
        loc_pop_footer.append(&location_map_copy_btn);
        loc_pop_footer.append(&location_map_open_btn);
        loc_pop_vbox.append(&loc_pop_footer);

        let location_map_popover = gtk::Popover::builder()
            .child(&loc_pop_vbox)
            .position(gtk::PositionType::Bottom)
            .has_arrow(false)
            .autohide(false)
            .css_classes(vec!["itip-location-map-popover"])
            .build();
        location_map_popover.set_parent(&location_row);
        {
            let pop_cleanup = location_map_popover.clone();
            location_row.connect_destroy(move |_| {
                pop_cleanup.unparent();
            });
        }

        // Hover & click controllers for minimalist location map preview
        {
            #[cfg(target_os = "linux")]
            let map_webview: Rc<RefCell<Option<webkit6::WebView>>> = Rc::new(RefCell::new(None));
            let last_loaded_loc = Rc::new(RefCell::new(String::new()));
            let ptr_in_row = Rc::new(Cell::new(false));
            let ptr_in_pop = Rc::new(Cell::new(false));
            let open_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            let close_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

            let schedule_close = {
                let ptr_in_row = ptr_in_row.clone();
                let ptr_in_pop = ptr_in_pop.clone();
                let close_timer = close_timer.clone();
                let pop = location_map_popover.clone();
                Rc::new(move || {
                    if let Some(id) = close_timer.borrow_mut().take() {
                        id.remove();
                    }
                    let r_in = ptr_in_row.clone();
                    let p_in = ptr_in_pop.clone();
                    let pop_c = pop.clone();
                    let timer_slot = close_timer.clone();
                    let id = glib::timeout_add_local_once(
                        std::time::Duration::from_millis(220),
                        move || {
                            *timer_slot.borrow_mut() = None;
                            if !r_in.get() && !p_in.get() {
                                pop_c.popdown();
                            }
                        },
                    );
                    *close_timer.borrow_mut() = Some(id);
                })
            };

            let row_motion = gtk::EventControllerMotion::new();
            let ptr_in_row_enter = ptr_in_row.clone();
            let close_timer_enter = close_timer.clone();
            let open_timer_enter = open_timer.clone();
            let loc_text_enter = current_location_text.clone();
            let loc_geo_enter = current_location_geo.clone();
            let pop_enter = location_map_popover.clone();
            let open_btn_enter = location_map_open_btn.clone();
            #[cfg(target_os = "linux")]
            let frame_enter = location_map_frame.clone();
            #[cfg(target_os = "linux")]
            let wv_enter = map_webview.clone();
            let last_loaded_enter = last_loaded_loc.clone();

            row_motion.connect_enter(move |_, _, _| {
                ptr_in_row_enter.set(true);
                if let Some(id) = close_timer_enter.borrow_mut().take() {
                    id.remove();
                }
                let hover_enabled = AppSettings::try_new()
                    .map(|s| s.calendar_location_map_hover())
                    .unwrap_or(true);
                if !hover_enabled {
                    return;
                }
                let loc = loc_text_enter.borrow().clone();
                if loc.is_empty() || loc.starts_with("http://") || loc.starts_with("https://") {
                    return;
                }
                if let Some(id) = open_timer_enter.borrow_mut().take() {
                    id.remove();
                }
                let r_in = ptr_in_row_enter.clone();
                let pop_c = pop_enter.clone();
                let open_btn_c = open_btn_enter.clone();
                let loc_c = loc.clone();
                let geo_c = loc_geo_enter.get();
                let last_loaded_c = last_loaded_enter.clone();
                #[cfg(target_os = "linux")]
                let frame_c = frame_enter.clone();
                #[cfg(target_os = "linux")]
                let wv_c = wv_enter.clone();
                let timer_slot = open_timer_enter.clone();

                let id = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(220),
                    move || {
                        *timer_slot.borrow_mut() = None;
                        if !r_in.get() {
                            return;
                        }
                        let can_open = AppSettings::try_new()
                            .map(|s| s.calendar_location_open_map())
                            .unwrap_or(true);
                        open_btn_c.set_visible(can_open);

                        #[cfg(target_os = "linux")]
                        {
                            if wv_c.borrow().is_none() {
                                let view = webkit6::WebView::builder()
                                    .hexpand(true)
                                    .vexpand(true)
                                    .width_request(268)
                                    .height_request(152)
                                    .css_classes(vec!["itip-location-map-webview"])
                                    .build();
                                frame_c.set_child(Some(&view));
                                *wv_c.borrow_mut() = Some(view);
                            }
                            let is_dark = adw::StyleManager::default().is_dark();
                            let cache_key = format!("{loc_c}|{geo_c:?}|{is_dark}");
                            if *last_loaded_c.borrow() != cache_key {
                                *last_loaded_c.borrow_mut() = cache_key;
                                if let Some(view) = wv_c.borrow().as_ref() {
                                    let html = build_minimal_map_hover_html(&loc_c, geo_c, is_dark);
                                    view.load_html(
                                        &html,
                                        Some("https://nominatim.openstreetmap.org/"),
                                    );
                                }
                            }
                        }
                        let _ = (&last_loaded_c, geo_c);
                        pop_c.popup();
                    },
                );
                *open_timer_enter.borrow_mut() = Some(id);
            });

            let ptr_in_row_leave = ptr_in_row.clone();
            let open_timer_leave = open_timer.clone();
            let sched_close_row = schedule_close.clone();
            row_motion.connect_leave(move |_| {
                ptr_in_row_leave.set(false);
                if let Some(id) = open_timer_leave.borrow_mut().take() {
                    id.remove();
                }
                sched_close_row();
            });
            location_row.add_controller(row_motion);

            let pop_motion = gtk::EventControllerMotion::new();
            let ptr_in_pop_enter = ptr_in_pop.clone();
            let close_timer_pop = close_timer.clone();
            pop_motion.connect_enter(move |_, _, _| {
                ptr_in_pop_enter.set(true);
                if let Some(id) = close_timer_pop.borrow_mut().take() {
                    id.remove();
                }
            });
            let ptr_in_pop_leave = ptr_in_pop.clone();
            let sched_close_pop = schedule_close;
            pop_motion.connect_leave(move |_| {
                ptr_in_pop_leave.set(false);
                sched_close_pop();
            });
            location_map_popover.add_controller(pop_motion);

            // Click on location row to open in map (if enabled in settings)
            let click_gesture = gtk::GestureClick::new();
            let loc_for_click = current_location_text.clone();
            let geo_for_click = current_location_geo.clone();
            click_gesture.connect_released(move |_, _, _, _| {
                let open_enabled = AppSettings::try_new()
                    .map(|s| s.calendar_location_open_map())
                    .unwrap_or(true);
                if !open_enabled {
                    return;
                }
                let loc = loc_for_click.borrow().clone();
                if !loc.is_empty() && !loc.starts_with("http://") && !loc.starts_with("https://") {
                    open_calendar_location_in_map(&loc, geo_for_click.get());
                }
            });
            location_row.add_controller(click_gesture);

            let loc_for_open_btn = current_location_text.clone();
            let geo_for_open_btn = current_location_geo.clone();
            let pop_for_open_btn = location_map_popover.clone();
            location_map_open_btn.connect_clicked(move |_| {
                let loc = loc_for_open_btn.borrow().clone();
                if !loc.is_empty() {
                    open_calendar_location_in_map(&loc, geo_for_open_btn.get());
                }
                pop_for_open_btn.popdown();
            });

            let loc_for_copy_btn = current_location_text.clone();
            let pop_for_copy_btn = location_map_popover.clone();
            location_map_copy_btn.connect_clicked(move |btn| {
                let loc = loc_for_copy_btn.borrow().clone();
                if !loc.is_empty() {
                    btn.display().clipboard().set_text(&loc);
                }
                pop_for_copy_btn.popdown();
            });
        }

        let organizer_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let org_icon = gtk::Image::builder()
            .icon_name("avatar-default-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_organizer = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(vec!["caption"])
            .build();
        organizer_row.append(&org_icon);
        organizer_row.append(&lbl_organizer);
        details_info_box.append(&organizer_row);

        let attendees_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let att_icon = gtk::Image::builder()
            .icon_name("system-users-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_attendees = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .ellipsize(pango::EllipsizeMode::End)
            .lines(2)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        attendees_row.append(&att_icon);
        attendees_row.append(&lbl_attendees);
        details_info_box.append(&attendees_row);

        details_vbox.append(&details_info_box);

        // 3. Sender Message / Description rendered inside a proper Card (like mail content)
        let sender_message_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(2)
            .margin_bottom(8)
            .css_classes(vec!["card", "mail-preview-card", "itip-sender-message-box"])
            .visible(false)
            .build();
        let lbl_sender_message_title = gtk::Label::builder()
            .label(&gettext("Message from sender:"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        sender_message_box.append(&lbl_sender_message_title);
        let lbl_sender_message = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .can_focus(false)
            .css_classes(vec!["body", "itip-sender-message-text"])
            .build();
        sender_message_box.append(&lbl_sender_message);
        details_vbox.append(&sender_message_box);

        let exp_description = gtk::Expander::builder().visible(false).build();
        let lbl_description = lbl_sender_message.clone();

        // 4. Calendar Presence Status (internal, hidden from main view to avoid clutter)
        let calendar_status_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let calendar_status_icon = gtk::Image::builder()
            .icon_name("dialog-information-symbolic")
            .pixel_size(18)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_calendar_status = gtk::Label::builder()
            .label("")
            .build();
        calendar_status_box.append(&calendar_status_icon);
        calendar_status_box.append(&lbl_calendar_status);

        // 5. Advanced Controls (Target Calendar, Checkboxes, Import as Event)
        let cal_select_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();
        let cal_select_lbl = gtk::Label::builder()
            .label(&format!("{}:", gettext("Calendar")))
            .css_classes(vec!["caption", "dim-label"])
            .build();
        cal_select_box.append(&cal_select_lbl);
        let calendar_dropdown = gtk::DropDown::builder()
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        cal_select_box.append(&calendar_dropdown);

        let chk_send_reply = gtk::CheckButton::builder()
            .label(&gettext("Send reply to organizer"))
            .active(true)
            .build();

        let chk_show_free = gtk::CheckButton::builder()
            .label(&gettext("Show time as free"))
            .active(false)
            .build();
        let chk_preserve_reminder = gtk::CheckButton::builder()
            .label(&gettext("Preserve my reminder"))
            .active(true)
            .build();

        let btn_import_as_event = gtk::Button::builder()
            .label(&gettext("Import as Event"))
            .css_classes(vec!["flat"])
            .halign(gtk::Align::Start)
            .build();
        let lbl_import_hint = gtk::Label::builder()
            .label(&gettext("Imported event will have removed the organizer and all the attendees."))
            .css_classes(vec!["caption", "dim-label"])
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .width_chars(1)
            .build();

        // 6. Comment Box (Progressive Disclosure - Revealed on demand right above sticky footer)
        let comment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(6)
            .build();
        let lbl_comment = gtk::Label::builder()
            .label(&gettext("Add a note to your response:"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        comment_box.append(&lbl_comment);
        let comment_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(48)
            .css_classes(vec!["itip-comment-view"])
            .build();
        comment_box.append(&comment_view);

        let comment_revealer = gtk::Revealer::builder()
            .reveal_child(false)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(150)
            .child(&comment_box)
            .build();

        // 7. Top Action Bar (Left: Primary RSVP Actions + Comment; Right: Target Calendar Chip above mini-calendar)
        let footer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(8)
            .build();

        let secondary_bar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();

        let calendar_chip_list_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_top(1)
            .margin_bottom(1)
            .hexpand(true)
            .build();
        let calendar_chip_scrolled = gtk::ScrolledWindow::builder()
            .max_content_height(800)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&calendar_chip_list_box)
            .build();
        let chip_pop_outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_start(2)
            .margin_end(2)
            .margin_top(2)
            .margin_bottom(2)
            .width_request(256)
            .build();
        chip_pop_outer.append(&calendar_chip_scrolled);
        let calendar_chip_popover = gtk::Popover::builder()
            .css_classes(vec!["calendar-picker-popover", "composer-dropdown"])
            .child(&chip_pop_outer)
            .has_arrow(false)
            .autohide(true)
            .build();
        {
            let scrolled_for_pop = calendar_chip_scrolled.clone();
            calendar_chip_popover.connect_show(move |_| {
                let vadj = scrolled_for_pop.vadjustment();
                vadj.set_value(0.0);
                glib::idle_add_local_once(move || {
                    vadj.set_value(0.0);
                });
            });
        }

        let chip_inner_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(7)
            .hexpand(true)
            .valign(gtk::Align::Fill)
            .build();
        let lbl_calendar_dot = gtk::Box::builder()
            .width_request(5)
            .height_request(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(vec!["itip-calendar-color-dot"])
            .build();
        apply_calendar_dot_style(&lbl_calendar_dot, "#3584e4");
        let lbl_calendar_chip_name = gtk::Label::builder()
            .label(&gettext("Personal Calendar"))
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .hexpand(true)
            .xalign(0.0)
            .yalign(0.5)
            .single_line_mode(true)
            .valign(gtk::Align::Fill)
            .build();
        let chip_chevron = gtk::Image::builder()
            .icon_name("pan-down-symbolic")
            .pixel_size(12)
            .css_classes(vec!["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        chip_inner_box.append(&lbl_calendar_dot);
        chip_inner_box.append(&lbl_calendar_chip_name);
        chip_inner_box.append(&chip_chevron);

        let btn_target_calendar = gtk::MenuButton::builder()
            .child(&chip_inner_box)
            .popover(&calendar_chip_popover)
            .tooltip_text(&gettext("Target Calendar"))
            .css_classes(vec!["flat", "itip-calendar-chip"])
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Center)
            .build();

        secondary_bar_box.append(&btn_target_calendar);

        let btn_open_calendar = gtk::Button::builder()
            .label(&gettext("Open in Calendar"))
            .css_classes(vec!["flat"])
            .tooltip_text(&gettext("Switch to calendar and show date"))
            .visible(false)
            .build();

        // GNOME HIG: show text-only label when enough horizontal room, switch to icon-only when narrow
        let comment_btn_inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .valign(gtk::Align::Center)
            .build();
        let btn_comment_icon = gtk::Image::builder()
            .icon_name("user-available-symbolic")
            .visible(false)
            .valign(gtk::Align::Center)
            .build();
        let btn_comment_lbl = gtk::Label::builder()
            .label(&gettext("Comment"))
            .single_line_mode(true)
            .valign(gtk::Align::Center)
            .build();
        comment_btn_inner.append(&btn_comment_icon);
        comment_btn_inner.append(&btn_comment_lbl);

        let btn_toggle_comment = gtk::ToggleButton::builder()
            .child(&comment_btn_inner)
            .css_classes(vec!["flat"])
            .tooltip_text(&gettext("Add a comment to your response"))
            .valign(gtk::Align::Center)
            .build();
        let rev_comment = comment_revealer.clone();
        let view_comment = comment_view.clone();
        btn_toggle_comment.connect_toggled(move |btn| {
            let active = btn.is_active();
            rev_comment.set_reveal_child(active);
            if active {
                view_comment.grab_focus();
            }
        });

        let sep_footer_inner = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep_footer_inner.set_hexpand(true);
        sep_footer_inner.add_css_class("itip-footer-inner-sep");
        sep_footer_inner.set_visible(false);

        // ── Primary RSVP Decision Buttons (Left-Aligned) ──────────────────────
        let primary_actions_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();

        // Request actions
        let action_box_request = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();

        let make_action_btn = |icon_name: &str,
                               text: &str,
                               classes: Vec<&str>,
                               tooltip: &str|
         -> (gtk::Button, gtk::Image, gtk::Label) {
            let inner = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            let icon = gtk::Image::builder()
                .icon_name(icon_name)
                .pixel_size(14)
                .visible(false)
                .valign(gtk::Align::Center)
                .build();
            let lbl = gtk::Label::builder()
                .label(text)
                .single_line_mode(true)
                .valign(gtk::Align::Center)
                .build();
            inner.append(&icon);
            inner.append(&lbl);
            let btn = gtk::Button::builder()
                .child(&inner)
                .css_classes(classes)
                .tooltip_text(tooltip)
                .build();
            (btn, icon, lbl)
        };

        let (btn_accept, btn_accept_icon, btn_accept_lbl) = make_action_btn(
            "object-select-symbolic",
            &gettext("Accept"),
            vec!["suggested-action"],
            &gettext("Accept event, add to calendar and send response"),
        );
        let (btn_tentative, btn_tentative_icon, btn_tentative_lbl) = make_action_btn(
            "dialog-question-symbolic",
            &gettext("Tentative"),
            vec![],
            &gettext("Tentatively accept and add to calendar"),
        );
        let (btn_decline, btn_decline_icon, btn_decline_lbl) = make_action_btn(
            "window-close-symbolic",
            &gettext("Decline"),
            vec!["destructive-action"],
            &gettext("Decline invitation and send response"),
        );
        action_box_request.append(&btn_accept);
        action_box_request.append(&btn_tentative);
        action_box_request.append(&btn_decline);
        action_box_request.append(&btn_toggle_comment);

        // More options popover (Expert options: Reply toggle, Free/Busy, Import as Event)
        let more_popover = gtk::Popover::new();
        let pop_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .width_request(250)
            .build();

        let opts_lbl = gtk::Label::builder()
            .label(&gettext("Options"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading", "dim-label"])
            .build();
        pop_box.append(&opts_lbl);
        pop_box.append(&chk_send_reply);
        pop_box.append(&chk_show_free);

        let sep_pop2 = gtk::Separator::new(gtk::Orientation::Horizontal);
        pop_box.append(&sep_pop2);

        pop_box.append(&btn_import_as_event);
        pop_box.append(&lbl_import_hint);

        more_popover.set_child(Some(&pop_box));

        let pop_close = more_popover.clone();
        btn_import_as_event.connect_clicked(move |_| {
            pop_close.popdown();
        });

        let btn_more_options = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&more_popover)
            .tooltip_text(&gettext("More options"))
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        action_box_request.append(&btn_more_options);
        primary_actions_row.append(&action_box_request);

        // Cancel actions (Single-row layout: Remove button on left + inline cancellation notice on right)
        let action_box_cancel = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let btn_remove = gtk::Button::builder()
            .label(&gettext("Remove from Calendar"))
            .css_classes(vec!["destructive-action"])
            .valign(gtk::Align::Center)
            .tooltip_text(&gettext("Remove cancelled event from your calendar"))
            .build();
        let cancel_inline_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let cancel_inline_icon = gtk::Image::builder()
            .icon_name("process-stop-symbolic")
            .pixel_size(16)
            .css_classes(vec!["error"])
            .valign(gtk::Align::Center)
            .build();
        let lbl_cancel_inline = gtk::Label::builder()
            .label(&gettext("Meeting has been cancelled"))
            .tooltip_text(&gettext("Meeting has been cancelled"))
            .css_classes(vec!["caption", "heading", "dim-label"])
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .valign(gtk::Align::Center)
            .build();
        cancel_inline_box.append(&cancel_inline_icon);
        cancel_inline_box.append(&lbl_cancel_inline);
        action_box_cancel.append(&btn_remove);
        action_box_cancel.append(&cancel_inline_box);
        primary_actions_row.append(&action_box_cancel);

        // Reply actions (Single-row layout: Update button on left + inline reply notice on right)
        let action_box_reply = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let btn_update = gtk::Button::builder()
            .label(&gettext("Update Calendar"))
            .css_classes(vec!["suggested-action"])
            .valign(gtk::Align::Center)
            .tooltip_text(&gettext("Update attendee status in existing calendar event"))
            .build();
        let reply_inline_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let reply_inline_icon = gtk::Image::builder()
            .icon_name("mail-reply-sender-symbolic")
            .pixel_size(16)
            .css_classes(vec!["accent"])
            .valign(gtk::Align::Center)
            .build();
        let lbl_reply_inline = gtk::Label::builder()
            .label(&gettext("Response to Meeting Invitation"))
            .tooltip_text(&gettext("Response to Meeting Invitation"))
            .css_classes(vec!["caption", "heading", "dim-label"])
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .valign(gtk::Align::Center)
            .build();
        reply_inline_box.append(&reply_inline_icon);
        reply_inline_box.append(&lbl_reply_inline);
        action_box_reply.append(&btn_update);
        action_box_reply.append(&reply_inline_box);
        primary_actions_row.append(&action_box_reply);

        // Publish actions
        let action_box_publish = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let btn_import = gtk::Button::builder()
            .label(&gettext("Add to Calendar"))
            .css_classes(vec!["suggested-action"])
            .tooltip_text(&gettext("Add published event to calendar"))
            .build();
        action_box_publish.append(&btn_import);
        primary_actions_row.append(&action_box_publish);

        footer_box.append(&primary_actions_row);
        footer_box.append(&sep_footer_inner);
        footer_box.append(&secondary_bar_box);

        // ── Right Column: Day Schedule / Timeline Strip (Calendar Day View Parity) ──
        let timeline_side_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .width_request(280)
            .valign(gtk::Align::Fill)
            .css_classes(vec!["itip-day-timeline-container", "calendar-time-grid"])
            .visible(false)
            .build();

        // Hidden in UI because the full date + time is already the primary heading on the left column at the exact same height
        let timeline_top_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .css_classes(vec!["calendar-header-grid", "itip-timeline-header"])
            .visible(false)
            .build();

        let lbl_timeline_date = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(vec!["heading"])
            .build();
        timeline_top_bar.append(&lbl_timeline_date);

        let lbl_timeline_badge = gtk::Label::builder()
            .label("")
            .css_classes(vec!["appointment-timeline-conflict-badge"])
            .visible(false)
            .build();
        // NOTE: lbl_timeline_badge is kept for test compatibility; conflicts are shown directly under the event time in details_info_box.
        timeline_side_box.append(&timeline_top_bar);

        let timeline_all_day_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .css_classes(vec!["calendar-allday-row", "itip-timeline-allday-row"])
            .visible(false)
            .build();
        timeline_side_box.append(&timeline_all_day_box);

        let timeline_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .height_request(340)
            .vexpand(true)
            .css_classes(vec!["itip-timeline-scrolled"])
            .build();

        let timeline_overlay = gtk::Overlay::new();
        let timeline_hours_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .hexpand(true)
            .build();

        let timeline_hour_rows: Rc<RefCell<Vec<(gtk::Box, gtk::Box)>>> =
            Rc::new(RefCell::new(Vec::with_capacity(24)));

        for h in 0..24 {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(0)
                .height_request(HOUR_HEIGHT as i32)
                .hexpand(true)
                .css_classes(vec!["appointment-timeline-hour-row", "itip-timeline-hour-row"])
                .build();
            let gutter_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .width_request(44)
                .height_request(HOUR_HEIGHT as i32)
                .valign(gtk::Align::Fill)
                .css_classes(vec!["calendar-time-gutter", "itip-time-gutter"])
                .build();
            let lbl = gtk::Label::builder()
                .use_markup(true)
                .label(&crate::views::calendar_page::format_hour_markup_scaled((h * 60) as u32, false, 0.68))
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .margin_top(2)
                .css_classes(vec!["calendar-time-gutter-label", "itip-time-gutter-label"])
                .build();
            gutter_box.append(&lbl);

            let slot_area = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .hexpand(true)
                .valign(gtk::Align::Fill)
                .css_classes(vec!["itip-timeline-hour-slot"])
                .build();
            let sep_hour = gtk::Separator::builder()
                .orientation(gtk::Orientation::Horizontal)
                .hexpand(true)
                .valign(gtk::Align::Start)
                .build();
            slot_area.append(&sep_hour);

            row.append(&gutter_box);
            row.append(&slot_area);
            timeline_hours_box.append(&row);
            timeline_hour_rows.borrow_mut().push((row, gutter_box));
        }
        timeline_overlay.set_child(Some(&timeline_hours_box));

        let timeline_fixed = gtk::Fixed::builder()
            .height_request((24.0 * HOUR_HEIGHT) as i32)
            .hexpand(true)
            .build();
        timeline_overlay.add_overlay(&timeline_fixed);
        timeline_scrolled.set_child(Some(&timeline_overlay));
        timeline_side_box.append(&timeline_scrolled);

        let timeline_target_scroll_y = Rc::new(Cell::new(0.0));
        let vadj_map = timeline_scrolled.vadjustment();
        let target_y_for_map = Rc::clone(&timeline_target_scroll_y);
        timeline_scrolled.connect_map(move |_| {
            let y = target_y_for_map.get();
            let adj = vadj_map.clone();
            glib::idle_add_local_once(move || {
                adj.set_value(y);
            });
        });

        let details_columns = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .hexpand(true)
            .valign(gtk::Align::Fill)
            .build();
        details_columns.append(&details_vbox);
        details_columns.append(&timeline_side_box);

        let details_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .min_content_width(0)
            .hexpand(true)
            .vexpand(true)
            .child(&details_columns)
            .build();

        let details_revealer = gtk::Revealer::builder()
            .reveal_child(true)
            .hexpand(true)
            .vexpand(true)
            .transition_type(gtk::RevealerTransitionType::None)
            .child(&details_scrolled)
            .build();

        // Top Header Section: Header Row + Top Action Toolbar + Progressive Comment Box + Separator
        banner_box.append(&footer_box);
        banner_box.append(&comment_revealer);
        banner_box.append(&sep_top);
        banner_box.append(&details_revealer);
        revealer.set_child(Some(&banner_box));

        let timeline_hour_height = Rc::new(Cell::new(HOUR_HEIGHT));
        let timeline_placed_widgets: Rc<RefCell<Vec<PlacedTimelineWidget>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Responsive adaptation when the preview pane is narrowed or widened
        {
            let cols = details_columns.clone();
            let tl_box = timeline_side_box.clone();
            let tl_fixed = timeline_fixed.clone();
            let placed_for_resize = timeline_placed_widgets.clone();
            let hour_h_for_resize = timeline_hour_height.clone();
            let sender_msg_for_resize = sender_message_box.clone();
            let act_bar = footer_box.clone();
            let req_box = action_box_request.clone();
            let sec_bar = secondary_bar_box.clone();
            let comment_icon = btn_comment_icon.clone();
            let comment_lbl = btn_comment_lbl.clone();
            let acc_icon = btn_accept_icon.clone();
            let acc_lbl = btn_accept_lbl.clone();
            let tent_icon = btn_tentative_icon.clone();
            let tent_lbl = btn_tentative_lbl.clone();
            let dec_icon = btn_decline_icon.clone();
            let dec_lbl = btn_decline_lbl.clone();
            let last_width = Cell::new(0);
            let last_tl_width = Cell::new(0);
            let last_tl_vis = Cell::new(false);
            let last_msg_vis = Cell::new(false);
            let last_max_cols = Cell::new(1usize);
            banner_box.add_tick_callback(move |widget, _| {
                let w = widget.width();
                let tl_vis = tl_box.is_visible();
                let tl_w = tl_box.width();
                let msg_vis = sender_msg_for_resize.is_visible();
                let max_cols = placed_for_resize
                    .borrow()
                    .iter()
                    .map(|p| p.total_cols)
                    .max()
                    .unwrap_or(1);
                if w > 0
                    && (w != last_width.get()
                        || tl_vis != last_tl_vis.get()
                        || msg_vis != last_msg_vis.get()
                        || max_cols != last_max_cols.get())
                {
                    last_width.set(w);
                    last_tl_vis.set(tl_vis);
                    last_msg_vis.set(msg_vis);
                    last_max_cols.set(max_cols);
                    let stack_columns = w < 620;
                    if stack_columns {
                        if cols.orientation() != gtk::Orientation::Vertical {
                            cols.set_orientation(gtk::Orientation::Vertical);
                            cols.set_spacing(0);
                        }
                        tl_box.set_width_request(-1);
                        tl_box.set_hexpand(true);
                        tl_box.add_css_class("is-stacked");
                    } else {
                        if cols.orientation() != gtk::Orientation::Horizontal {
                            cols.set_orientation(gtk::Orientation::Horizontal);
                            cols.set_spacing(0);
                        }
                        // Minimum width is 280px; scale up with preview pane width (especially when
                        // metadata on the left is compact or when overlapping columns exist)
                        let target_tl_w =
                            compute_dynamic_timeline_width(w, !msg_vis, max_cols);
                        if tl_box.width_request() != target_tl_w {
                            tl_box.set_width_request(target_tl_w);
                        }
                        tl_box.set_hexpand(false);
                        tl_box.remove_css_class("is-stacked");
                    }

                    let stack_actions = w < 520;
                    if stack_actions {
                        if act_bar.orientation() != gtk::Orientation::Vertical {
                            act_bar.set_orientation(gtk::Orientation::Vertical);
                            act_bar.set_spacing(6);
                        }
                        sec_bar.set_halign(gtk::Align::Start);
                    } else {
                        if act_bar.orientation() != gtk::Orientation::Horizontal {
                            act_bar.set_orientation(gtk::Orientation::Horizontal);
                            act_bar.set_spacing(8);
                        }
                        sec_bar.set_halign(gtk::Align::End);
                    }

                    // Progressive mobile compaction so the action bar shrinks down to < 200px:
                    // 1. < 640px: "Comment" collapses to icon-only
                    let icon_only_comment = w < 640;
                    comment_lbl.set_visible(!icon_only_comment);
                    comment_icon.set_visible(icon_only_comment);

                    // 2. < 440px: tighter button margins & spacing for mobile screens
                    if w < 440 {
                        act_bar.add_css_class("is-mobile-compact");
                        act_bar.set_margin_start(8);
                        act_bar.set_margin_end(8);
                        req_box.set_spacing(4);
                    } else {
                        act_bar.remove_css_class("is-mobile-compact");
                        act_bar.set_margin_start(8);
                        act_bar.set_margin_end(8);
                        req_box.set_spacing(6);
                    }

                    // 3. < 390px: Tentative & Decline collapse to compact icon pills ([Accept] [?] [✕] [💬] [⋮])
                    let icon_only_secondary_rsvp = w < 390;
                    tent_lbl.set_visible(!icon_only_secondary_rsvp);
                    tent_icon.set_visible(icon_only_secondary_rsvp);
                    dec_lbl.set_visible(!icon_only_secondary_rsvp);
                    dec_icon.set_visible(icon_only_secondary_rsvp);

                    // 4. < 300px: Accept also collapses to icon-only ([✓] [?] [✕] [💬] [⋮])
                    let icon_only_accept = w < 300;
                    acc_lbl.set_visible(!icon_only_accept);
                    acc_icon.set_visible(icon_only_accept);
                }

                // Dynamically stretch timeline event blocks across the full available width when stacked or resized
                if tl_vis && tl_w > 60 && tl_w != last_tl_width.get() {
                    last_tl_width.set(tl_w);
                    let avail_w = (tl_w as f64 - 47.0 - 15.0).max(120.0);
                    let cur_h = hour_h_for_resize.get();
                    for item in placed_for_resize.borrow_mut().iter_mut() {
                        let total_cols = item.total_cols.max(1);
                        let col_gap = 2.0;
                        let col_width = if total_cols > 1 {
                            ((avail_w - (total_cols as f64 - 1.0) * col_gap) / total_cols as f64)
                                .max(32.0)
                        } else {
                            avail_w
                        };
                        let x = 47.0 + item.col_idx as f64 * (col_width + col_gap);
                        item.x = x;
                        item.widget.set_width_request(col_width.round() as i32);
                        let y = (item.start_min as f64 / 60.0) * cur_h;
                        tl_fixed.move_(&item.widget, x, y);
                    }
                }
                glib::ControlFlow::Continue
            });
        }

        let pointer_in_timeline = Rc::new(Cell::new(false));
        {
            let motion = gtk::EventControllerMotion::new();
            let ptr_enter = pointer_in_timeline.clone();
            motion.connect_enter(move |_, _, _| {
                ptr_enter.set(true);
            });
            let ptr_leave = pointer_in_timeline.clone();
            motion.connect_leave(move |_| {
                ptr_leave.set(false);
            });
            timeline_side_box.add_controller(motion);
        }

        let banner = Self {
            revealer,
            banner_box,
            header_row,
            details_revealer,
            details_scrolled: details_scrolled.clone(),
            details_zoom: Rc::new(Cell::new(1.0)),
            details_zoom_class,
            details_zoom_provider,
            pointer_in_timeline,
            cal_select_box,
            footer_box,
            is_compact: Rc::new(Cell::new(false)),
            collapse_guard_until: Rc::new(Cell::new(0)),
            full_header_prompt: Rc::new(RefCell::new(gettext("Meeting Invitation"))),
            icon,
            lbl_header_prompt,
            lbl_cancel_inline,
            lbl_title,
            lbl_method,
            lbl_status_badge,
            lbl_time,
            conflict_row,
            conflict_icon,
            lbl_conflict,
            timeline_side_box: timeline_side_box.clone(),
            lbl_timeline_date,
            lbl_timeline_badge,
            timeline_all_day_box,
            timeline_hours_box,
            timeline_hour_rows,
            timeline_hour_height,
            timeline_placed_widgets,
            zoom_anchor_hour: Rc::new(Cell::new(None)),
            zoom_anchor_cleanup: Rc::new(RefCell::new(None)),
            timeline_fixed,
            timeline_scrolled,
            timeline_target_scroll_y,
            last_timeline_state: Rc::new(RefCell::new(None)),
            lbl_location,
            location_map_popover,
            location_map_lbl,
            location_map_open_btn,
            current_location_text,
            current_location_geo,
            lbl_organizer,
            lbl_attendees,
            lbl_recurrence,
            lbl_description,
            exp_description,
            sender_message_box,
            lbl_sender_message,
            calendar_status_box,
            calendar_status_icon,
            lbl_calendar_status,
            chk_send_reply,
            comment_view,
            chk_show_free,
            chk_preserve_reminder,
            location_row,
            recurrence_row,
            organizer_row,
            attendees_row,
            calendar_dropdown,
            calendar_sources: Rc::new(RefCell::new(Vec::new())),
            calendar_picker_items: Rc::new(RefCell::new(Vec::new())),
            calendar_source_colors: Rc::new(RefCell::new(std::collections::HashMap::new())),
            secondary_bar_box,
            btn_target_calendar,
            lbl_calendar_dot,
            lbl_calendar_chip_name,
            calendar_chip_popover,
            calendar_chip_list_box,
            sep_footer_inner,
            user_selected_calendar: Rc::new(Cell::new(false)),
            last_user_emails: Rc::new(RefCell::new(Vec::new())),
            btn_accept,
            btn_tentative,
            btn_decline,
            btn_open_calendar,
            btn_remove,
            btn_update,
            btn_import,
            btn_import_as_event,
            lbl_import_hint,
            action_box_request,
            action_box_cancel,
            action_box_reply,
            action_box_publish,
            comment_revealer,
            btn_toggle_comment,
            btn_more_options,
            current_invitation: Rc::new(RefCell::new(None)),
            on_calendar_navigate: Rc::new(RefCell::new(None)),
            on_action_toast: Rc::new(RefCell::new(None)),
            on_reply_draft_saved: Rc::new(RefCell::new(None)),
            on_scroll_offset: Rc::new(RefCell::new(None)),
        };

        // Connect scroll offset on details_scrolled to compact header (and notify MailPage)
        // without oscillating when header compaction causes the content to fit on screen
        {
            let this = banner.clone();
            details_scrolled.vadjustment().connect_value_changed(move |adj| {
                let offset_y = adj.value();
                let now = glib::monotonic_time();
                let max_scroll = (adj.upper() - adj.page_size()).max(0.0);

                if !this.is_compact.get() && offset_y > 24.0 {
                    this.collapse_guard_until.set(now + 450_000);
                    this.set_compact(true);
                    if let Some(cb) = this.on_scroll_offset.borrow().as_ref() {
                        cb(offset_y.max(40.0));
                    }
                } else if this.is_compact.get() {
                    // Do not auto-expand while viewport is expanding during header collapse,
                    // or when the entire content now fits on screen without needing a scrollbar
                    if now >= this.collapse_guard_until.get() && max_scroll > 20.0 && offset_y < 10.0 {
                        this.set_compact(false);
                        if let Some(cb) = this.on_scroll_offset.borrow().as_ref() {
                            cb(offset_y);
                        }
                    } else if offset_y >= 15.0 {
                        if let Some(cb) = this.on_scroll_offset.borrow().as_ref() {
                            cb(offset_y);
                        }
                    }
                }
            });

            let wheel_ctrl = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            wheel_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let this_wheel = banner.clone();
            let adj_wheel = details_scrolled.vadjustment();
            wheel_ctrl.connect_scroll(move |controller, _dx, dy| {
                let state = controller.current_event_state();
                if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    return glib::Propagation::Proceed;
                }
                let now = glib::monotonic_time();
                let y = adj_wheel.value();
                let max_scroll = (adj_wheel.upper() - adj_wheel.page_size()).max(0.0);
                if dy > 0.0 && !this_wheel.is_compact.get() && (y > 5.0 || max_scroll > 5.0) {
                    this_wheel.collapse_guard_until.set(now + 450_000);
                    this_wheel.set_compact(true);
                    if let Some(cb) = this_wheel.on_scroll_offset.borrow().as_ref() {
                        cb(y.max(40.0));
                    }
                } else if dy < -0.1
                    && this_wheel.is_compact.get()
                    && y <= 2.0
                    && now >= this_wheel.collapse_guard_until.get()
                {
                    this_wheel.set_compact(false);
                    if let Some(cb) = this_wheel.on_scroll_offset.borrow().as_ref() {
                        cb(0.0);
                    }
                }
                glib::Propagation::Proceed
            });
            details_scrolled.add_controller(wheel_ctrl);
        }

        // Connect Ctrl + Scroll zoom on details_vbox (left column) and timeline_side_box (right column)
        {
            let left_zoom_ctrl = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            left_zoom_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let this_left = banner.clone();
            left_zoom_ctrl.connect_scroll(move |controller, _dx, dy| {
                let state = controller.current_event_state();
                let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                    && !state.contains(gtk::gdk::ModifierType::SUPER_MASK);
                if is_ctrl {
                    this_left.adjust_details_zoom_delta(dy);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            details_vbox.add_controller(left_zoom_ctrl);

            let zoom_ctrl = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            zoom_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let this = banner.clone();
            zoom_ctrl.connect_scroll(move |controller, _dx, dy| {
                let state = controller.current_event_state();
                let is_ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                    && !state.contains(gtk::gdk::ModifierType::SUPER_MASK);
                if is_ctrl {
                    this.adjust_timeline_zoom_delta(dy);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            timeline_side_box.add_controller(zoom_ctrl);
        }

        banner.setup_calendar_sources();
        banner.connect_signals();
        banner
    }

    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    pub fn set_compact(&self, compact: bool) {
        if self.is_compact.get() == compact {
            return;
        }
        self.is_compact.set(compact);
        // Never hide details_revealer: ItipBanner replaces the mail body view for calendar invitations.
        self.details_revealer.set_reveal_child(true);
        self.cal_select_box.set_visible(!compact);
        self.lbl_method.set_visible(!compact);
        let has_actions = self.footer_box.is_visible()
            && (self.action_box_request.is_visible()
                || self.action_box_cancel.is_visible()
                || self.action_box_reply.is_visible()
                || self.action_box_publish.is_visible());
        if compact {
            self.banner_box.add_css_class("is-compact");
            if has_actions {
                self.header_row.set_visible(false);
                self.footer_box.set_margin_top(6);
                self.footer_box.set_margin_bottom(6);
            } else {
                self.header_row.set_visible(true);
                self.header_row.set_margin_top(4);
                self.header_row.set_margin_bottom(4);
                self.icon.set_pixel_size(18);
                self.lbl_header_prompt.set_wrap(false);
                self.lbl_header_prompt.set_single_line_mode(true);
                self.lbl_header_prompt.set_ellipsize(pango::EllipsizeMode::End);
                let title = self.lbl_title.text();
                let prompt = self.full_header_prompt.borrow().clone();
                if !title.is_empty() && title != gettext("Event") {
                    self.lbl_header_prompt.set_text(&format!("{title} — {prompt}"));
                }
            }
        } else {
            self.banner_box.remove_css_class("is-compact");
            if has_actions {
                self.header_row.set_visible(false);
                self.footer_box.set_margin_top(8);
                self.footer_box.set_margin_bottom(8);
            } else {
                self.header_row.set_visible(true);
                self.header_row.set_margin_top(10);
                self.header_row.set_margin_bottom(6);
                self.footer_box.set_margin_top(0);
                self.footer_box.set_margin_bottom(10);
            }
            self.icon.set_pixel_size(24);
            self.lbl_header_prompt.set_single_line_mode(false);
            self.lbl_header_prompt.set_wrap(true);
            self.lbl_header_prompt.set_ellipsize(pango::EllipsizeMode::None);
            let prompt = self.full_header_prompt.borrow().clone();
            self.lbl_header_prompt.set_text(&prompt);
        }
    }

    pub fn connect_scroll_offset<F: Fn(f64) + 'static>(&self, f: F) {
        *self.on_scroll_offset.borrow_mut() = Some(Box::new(f));
    }

    pub fn handle_ctrl_scroll_delta(&self, dy: f64) {
        if self.timeline_side_box.is_visible() && self.pointer_in_timeline.get() {
            self.adjust_timeline_zoom_delta(dy);
        } else {
            self.adjust_details_zoom_delta(dy);
        }
    }

    pub fn adjust_details_zoom_delta(&self, dy: f64) {
        if dy.abs() < 0.001 {
            return;
        }
        let old_z = self.details_zoom.get();
        let delta_z = (-dy * 0.08).clamp(-0.20, 0.20);
        let new_z = (old_z + delta_z).clamp(0.75, 2.20);
        if (new_z - old_z).abs() < 0.01 {
            return;
        }
        self.apply_details_zoom(new_z);
    }

    pub fn apply_details_zoom(&self, zoom: f64) {
        let z = zoom.clamp(0.75, 2.20);
        self.details_zoom.set(z);
        if (z - 1.0).abs() < 0.01 {
            self.details_zoom_provider.load_from_string("");
        } else {
            let cls = &self.details_zoom_class;
            let heading_rem = 1.08 * z;
            let body_rem = 0.98 * z;
            let caption_rem = 0.84 * z;
            let css = format!(
                ".{cls} label.heading {{ font-size: {heading_rem:.3}rem; }}\n\
                 .{cls} label.body, .{cls} label.itip-sender-message-text {{ font-size: {body_rem:.3}rem; }}\n\
                 .{cls} label.caption {{ font-size: {caption_rem:.3}rem; }}\n"
            );
            self.details_zoom_provider.load_from_string(&css);
        }
    }

    pub fn adjust_timeline_zoom(&self, delta_steps: i32) {
        if delta_steps == 0 {
            return;
        }
        self.adjust_timeline_zoom_delta(-(delta_steps as f64));
    }

    pub fn adjust_timeline_zoom_delta(&self, dy: f64) {
        if dy.abs() < 0.001 {
            return;
        }
        let old_h = self.timeline_hour_height.get();
        let delta_h = (-dy * 10.0).clamp(-24.0, 24.0);
        let new_h = (old_h + delta_h).clamp(24.0, 120.0);
        if (new_h - old_h).abs() < 0.25 {
            return;
        }

        let vadj = self.timeline_scrolled.vadjustment();
        let page_h = vadj.page_size().max(320.0);

        // Anchor center hour at the start of a continuous zoom gesture so scroll never drifts
        let center_hour = if let Some(anchor) = self.zoom_anchor_hour.get() {
            anchor
        } else {
            let h = ((vadj.value() + page_h * 0.5) / old_h).clamp(0.0, 24.0);
            self.zoom_anchor_hour.set(Some(h));
            h
        };

        if let Some(source) = self.zoom_anchor_cleanup.borrow_mut().take() {
            let _ = unsafe { glib::ffi::g_source_remove(source.as_raw()) };
        }
        let anchor_cell = self.zoom_anchor_hour.clone();
        let cleanup_cell = self.zoom_anchor_cleanup.clone();
        let source = glib::timeout_add_local_once(
            std::time::Duration::from_millis(350),
            move || {
                cleanup_cell.replace(None);
                anchor_cell.set(None);
            },
        );
        *self.zoom_anchor_cleanup.borrow_mut() = Some(source);

        self.timeline_hour_height.set(new_h);
        let total_h = (24.0 * new_h).round() as i32;
        let row_h = new_h.round() as i32;
        self.timeline_hours_box.set_height_request(total_h);
        self.timeline_fixed.set_height_request(total_h);
        for (row, gutter) in self.timeline_hour_rows.borrow().iter() {
            row.set_height_request(row_h);
            gutter.set_height_request(row_h);
        }

        // Update existing event widgets in-place without destroying/recreating widgets
        for item in self.timeline_placed_widgets.borrow().iter() {
            let y = (item.start_min as f64 / 60.0) * new_h;
            let duration = item.end_min.saturating_sub(item.start_min);
            let h = ((duration.max(20) as f64) / 60.0) * new_h - 2.0;
            let max_lines = if h >= 34.0 {
                (((h - 6.0) / 16.0).floor() as i32).max(1)
            } else {
                1
            };
            let is_multiline = max_lines > 1;
            item.widget.set_wrap(is_multiline);
            item.widget.set_single_line_mode(!is_multiline);
            item.widget.set_lines(max_lines);
            item.widget.set_height_request(h.max(20.0).round() as i32);
            self.timeline_fixed.move_(&item.widget, item.x, y);
        }

        let new_upper = 24.0 * new_h;
        vadj.set_upper(new_upper);
        let max_scroll = (new_upper - page_h).max(0.0);
        let new_scroll = (center_hour * new_h - page_h * 0.5).clamp(0.0, max_scroll);
        self.timeline_target_scroll_y.set(new_scroll);
        vadj.set_value(new_scroll);
    }

    pub fn connect_calendar_navigate<F: Fn(NaiveDate) + 'static>(&self, f: F) {
        *self.on_calendar_navigate.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_action_toast<F: Fn(String) + 'static>(&self, f: F) {
        *self.on_action_toast.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_reply_draft_saved<
        F: Fn(&ItipInvitation, ItipReplyStatus, Option<String>, bool) + 'static,
    >(
        &self,
        f: F,
    ) {
        *self.on_reply_draft_saved.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_email_body(&self, body: Option<&str>) {
        if let Some(body_text) = body {
            let clean = body_text.trim();
            if !clean.is_empty() {
                self.lbl_sender_message.set_text(clean);
                self.lbl_description.set_text(clean);
                self.sender_message_box.set_visible(true);
                return;
            }
        }
        if let Some(inv) = self.current_invitation.borrow().as_ref() {
            let desc = inv.event.description.trim();
            if !desc.is_empty() {
                self.lbl_sender_message.set_text(desc);
                self.lbl_description.set_text(desc);
                self.sender_message_box.set_visible(true);
                return;
            }
        }
        self.lbl_sender_message.set_text("");
        self.sender_message_box.set_visible(false);
    }

    fn comment_text(&self) -> Option<String> {
        let buf = self.comment_view.buffer();
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn update_calendar_presence_status(
        &self,
        event: &crate::services::calendar_store_service::CalendarEventRecord,
        snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
    ) {
        if let Some(existing) = snapshot.events.iter().find(|e| !e.uid.is_empty() && e.uid == event.uid) {
            let cal_name = if !existing.source_display_name.is_empty() {
                &existing.source_display_name
            } else {
                &existing.source_uid
            };
            let status_desc = if existing.component.contains("PARTSTAT=ACCEPTED") {
                gettext("accepted")
            } else if existing.component.contains("PARTSTAT=TENTATIVE") {
                gettext("tentative")
            } else if existing.component.contains("PARTSTAT=DECLINED") {
                gettext("declined")
            } else {
                gettext("confirmed")
            };
            self.calendar_status_icon.set_icon_name(Some("object-select-symbolic"));
            self.calendar_status_icon.remove_css_class("dim-label");
            self.calendar_status_icon.add_css_class("success");
            let base = gettext("Found the appointment in the calendar \"{}\" as {}");
            self.lbl_calendar_status.set_text(&base.replacen("{}", cal_name, 1).replacen("{}", &status_desc, 1));
        } else {
            self.calendar_status_icon.set_icon_name(Some("dialog-information-symbolic"));
            self.calendar_status_icon.remove_css_class("success");
            self.calendar_status_icon.add_css_class("dim-label");
            self.lbl_calendar_status.set_text(&gettext("Appointment is not yet in your calendar"));
        }
    }

    fn resolve_source_color(&self, uid: &str, name: &str) -> String {
        if let Some(c) = self.calendar_source_colors.borrow().get(uid) {
            if !c.trim().is_empty() {
                return c.clone();
            }
        }
        crate::views::calendar_page::calendar_source_color(uid, name)
    }

    fn sync_calendar_chip_ui(&self) {
        let sources = self.calendar_sources.borrow().clone();
        let picker_items = self.calendar_picker_items.borrow().clone();
        let selected_idx = (self.calendar_dropdown.selected() as usize)
            .min(sources.len().saturating_sub(1));

        if let Some((uid, name)) = sources.get(selected_idx) {
            let color = self.resolve_source_color(uid, name);
            apply_calendar_dot_style(&self.lbl_calendar_dot, &color);
            self.lbl_calendar_chip_name.set_text(name);
            self.btn_target_calendar
                .set_tooltip_text(Some(&format!("{}: {}", gettext("Target Calendar"), name)));
        }

        while let Some(child) = self.calendar_chip_list_box.first_child() {
            self.calendar_chip_list_box.remove(&child);
        }

        let row_widgets: Rc<RefCell<Vec<(usize, gtk::Image, gtk::Button)>>> =
            Rc::new(RefCell::new(Vec::new()));

        let is_dark = adw::StyleManager::default().is_dark();
        let text_contrast_class = if is_dark {
            "itip-calendar-row-label-dark"
        } else {
            "itip-calendar-row-label-light"
        };

        // Collect all rows with their resolved group and clean name, then place the currently selected
        // calendar's account group at the top of the dropdown so it is immediately visible on open.
        let mut resolved_rows: Vec<(usize, String, String, String, String)> = Vec::new();
        let mut group_order: Vec<String> = Vec::new();
        let mut selected_group: Option<String> = None;

        for (idx, (uid, picker_label)) in sources.iter().enumerate() {
            let (group_name, row_label) = picker_items
                .get(idx)
                .filter(|item| &item.uid == uid)
                .map(|item| (item.group_name.clone(), item.clean_name.clone()))
                .unwrap_or_else(|| {
                    let g = gettext("On This Computer");
                    let c = crate::dialogs::event_dialog::clean_calendar_display_name(
                        picker_label,
                        &g,
                    );
                    (g, c)
                });
            if idx == selected_idx {
                selected_group = Some(group_name.clone());
            }
            if !group_order.contains(&group_name) {
                group_order.push(group_name.clone());
            }
            resolved_rows.push((
                idx,
                uid.clone(),
                picker_label.clone(),
                group_name,
                row_label,
            ));
        }

        if let Some(ref sel_grp) = selected_group {
            group_order.sort_by_key(|g| if g == sel_grp { 0usize } else { 1usize });
        }

        let mut ordered_rows: Vec<(usize, String, String, String, String)> = Vec::new();
        for grp in &group_order {
            for item in &resolved_rows {
                if &item.3 == grp {
                    ordered_rows.push(item.clone());
                }
            }
        }

        let mut last_group: Option<String> = None;

        for (idx, uid, picker_label, group_name, row_label) in ordered_rows {
            let is_selected = idx == selected_idx;
            let color = self.resolve_source_color(&uid, &picker_label);

            if last_group.as_deref() != Some(group_name.as_str()) {
                if last_group.is_some() {
                    let sep = gtk::Separator::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .css_classes(vec!["itip-calendar-group-sep"])
                        .build();
                    self.calendar_chip_list_box.append(&sep);
                }
                let hdr = gtk::Label::builder()
                    .label(&group_name)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .css_classes(vec!["itip-calendar-group-header"])
                    .build();
                self.calendar_chip_list_box.append(&hdr);
                last_group = Some(group_name.clone());
            }

            let row_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(9)
                .hexpand(true)
                .valign(gtk::Align::Fill)
                .build();

            let dot = gtk::Box::builder()
                .width_request(5)
                .height_request(16)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(vec!["itip-calendar-color-dot"])
                .build();
            apply_calendar_dot_style(&dot, &color);

            let lbl = gtk::Label::builder()
                .label(&row_label)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .yalign(0.5)
                .margin_top(2)
                .margin_bottom(2)
                .hexpand(true)
                .valign(gtk::Align::Fill)
                .css_classes(vec!["itip-calendar-row-label", text_contrast_class])
                .build();

            let check = gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .pixel_size(14)
                .opacity(if is_selected { 1.0 } else { 0.0 })
                .css_classes(vec!["accent"])
                .valign(gtk::Align::Center)
                .build();

            row_box.append(&dot);
            row_box.append(&lbl);
            row_box.append(&check);

            let mut classes = vec!["flat", "itip-calendar-chip-row"];
            if is_selected {
                classes.push("is-selected");
            }
            let row_btn = gtk::Button::builder()
                .child(&row_box)
                .css_classes(classes)
                .hexpand(true)
                .can_focus(false)
                .focus_on_click(false)
                .tooltip_text(&picker_label)
                .build();

            row_widgets
                .borrow_mut()
                .push((idx, check.clone(), row_btn.clone()));

            let this = self.clone();
            let pop = self.calendar_chip_popover.clone();
            let rows_ref = Rc::clone(&row_widgets);
            let item_color = color.clone();
            let item_name = picker_label.clone();
            row_btn.connect_clicked(move |_| {
                this.user_selected_calendar.set(true);
                this.calendar_dropdown.set_selected(idx as u32);
                apply_calendar_dot_style(&this.lbl_calendar_dot, &item_color);
                this.lbl_calendar_chip_name.set_text(&item_name);
                this.btn_target_calendar.set_tooltip_text(Some(&format!(
                    "{}: {}",
                    gettext("Target Calendar"),
                    item_name
                )));
                for (r_idx, r_check, r_btn) in rows_ref.borrow().iter() {
                    if *r_idx == idx {
                        r_check.set_opacity(1.0);
                        r_btn.add_css_class("is-selected");
                    } else {
                        r_check.set_opacity(0.0);
                        r_btn.remove_css_class("is-selected");
                    }
                }
                pop.popdown();
            });

            self.calendar_chip_list_box.append(&row_btn);
        }
    }

    fn apply_smart_calendar_selection(
        &self,
        snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
    ) {
        if self.user_selected_calendar.get() {
            return;
        }
        let inv_opt = self.current_invitation.borrow().clone();
        let Some(invitation) = inv_opt else {
            self.sync_calendar_chip_ui();
            return;
        };
        let sources = self.calendar_sources.borrow().clone();
        let user_emails = self.last_user_emails.borrow().clone();
        let user_email_refs: Vec<&str> = user_emails.iter().map(|s| s.as_str()).collect();

        if let Some(best_idx) = find_best_calendar_source_index(
            &sources,
            &invitation,
            &user_email_refs,
            snapshot,
        ) {
            self.calendar_dropdown.set_selected(best_idx as u32);
        }
        self.sync_calendar_chip_ui();
    }

    fn apply_discovered_calendar_sources(
        &self,
        discovered: Vec<crate::services::eds_service::EdsSource>,
    ) {
        let mut raw_items: Vec<(String, String, Option<String>)> = Vec::new();
        {
            let mut color_map = self.calendar_source_colors.borrow_mut();
            for source in discovered {
                if let Some(ref c) = source.color {
                    if !c.trim().is_empty() {
                        color_map.insert(source.uid.clone(), c.trim().to_string());
                    }
                }
                if source.enabled
                    && source.writable
                    && source.backend_name != "contacts"
                    && source.uid != "birthdays"
                {
                    raw_items.push((source.uid, source.display_name, source.parent_name));
                }
            }
        }

        if raw_items.is_empty() {
            for collection in crate::services::eds_service::load_configured_account_collections() {
                if !collection.calendar_enabled {
                    continue;
                }
                let account_name = collection.display_name.clone();
                let cal_uid = format!("account-calendar-{}", collection.uid);
                raw_items.push((
                    cal_uid,
                    gettext("Calendar"),
                    (!account_name.is_empty()).then_some(account_name),
                ));
            }
        }

        let picker_items = group_and_sort_calendar_sources(raw_items);
        let sources: Vec<(String, String)> = picker_items
            .iter()
            .map(|item| (item.uid.clone(), item.picker_label.clone()))
            .collect();

        let prev_uid = self.selected_source_uid();
        let names: Vec<&str> = sources.iter().map(|(_, name)| name.as_str()).collect();
        let model = gtk::StringList::new(&names);
        self.calendar_dropdown.set_model(Some(&model));
        if let Some(pos) = sources.iter().position(|(u, _)| u == &prev_uid) {
            self.calendar_dropdown.set_selected(pos as u32);
        } else {
            self.calendar_dropdown.set_selected(0);
        }
        *self.calendar_picker_items.borrow_mut() = picker_items;
        *self.calendar_sources.borrow_mut() = sources;
        let snapshot = load_local_calendar_snapshot_safe();
        self.apply_smart_calendar_selection(&snapshot);
    }

    fn setup_calendar_sources(&self) {
        // 1. Immediately populate from local EDS source cache so all configured calendars are ready synchronously
        let cached_sources = crate::services::eds_source_cache_service::EdsSourceCache::new()
            .load(crate::services::eds_service::EdsSourceKind::Calendar);
        self.apply_discovered_calendar_sources(cached_sources);

        // 2. Refresh asynchronously from live EDS
        let this = self.clone();
        glib::spawn_future_local(async move {
            let discovery = crate::services::eds_service::discover_sources(
                crate::services::eds_service::EdsSourceKind::Calendar,
            )
            .await;
            if !discovery.sources.is_empty() {
                this.apply_discovered_calendar_sources(discovery.sources);
            }
        });
    }

    fn selected_source_uid(&self) -> String {
        let index = self.calendar_dropdown.selected() as usize;
        let sources = self.calendar_sources.borrow();
        sources
            .get(index)
            .map(|(uid, _)| uid.clone())
            .unwrap_or_else(|| "local-calendar".to_string())
    }

    pub fn set_invitation(
        &self,
        invitation: Option<ItipInvitation>,
        user_emails: &[&str],
    ) {
        *self.current_invitation.borrow_mut() = invitation.clone();
        *self.last_user_emails.borrow_mut() =
            user_emails.iter().map(|s| (*s).to_string()).collect();
        self.user_selected_calendar.set(false);

        let Some(invitation) = invitation else {
            self.revealer.set_reveal_child(false);
            self.revealer.set_visible(false);
            self.revealer.set_vexpand(false);
            self.timeline_side_box.set_visible(false);
            return;
        };
        self.revealer.set_visible(true);
        self.revealer.set_vexpand(true);
        self.revealer.set_valign(gtk::Align::Fill);
        self.details_revealer.set_reveal_child(true);

        let event = &invitation.event;
        let title = if event.summary.trim().is_empty() {
            &gettext("Event without summary")
        } else {
            event.summary.trim()
        };
        self.lbl_title.set_text(title);

        // Method description and styling
        self.banner_box.remove_css_class("itip-cancelled");
        self.banner_box.remove_css_class("itip-accepted");
        self.icon.remove_css_class("accent");
        self.icon.remove_css_class("error");
        self.icon.remove_css_class("warning");
        self.icon.remove_css_class("success");

        let org_name = if let Some(p) = &invitation.organizer_person {
            p.display_label()
        } else if !invitation.organizer.trim().is_empty() {
            crate::services::itip_service::strip_mailto_prefix(invitation.organizer.trim()).to_string()
        } else {
            String::new()
        };

        match invitation.method {
            ItipMethod::Request | ItipMethod::Add => {
                self.icon.set_icon_name(Some("system-users-symbolic"));
                self.icon.add_css_class("accent");
                self.lbl_method.set_text(&gettext("Meeting Invitation"));
                if !org_name.is_empty() {
                    self.lbl_header_prompt.set_text(&gettext("{} requests your presence at the following meeting:").replace("{}", &org_name));
                } else {
                    self.lbl_header_prompt.set_text(&gettext("Meeting Invitation"));
                }
                self.action_box_request.set_visible(true);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
                self.secondary_bar_box.set_visible(true);
                self.btn_target_calendar.set_visible(true);
                self.btn_toggle_comment.set_visible(true);
                self.sep_footer_inner.set_visible(false);
                self.btn_toggle_comment.set_active(false);
                self.comment_revealer.set_reveal_child(false);
                self.comment_view.buffer().set_text("");
                self.chk_send_reply.set_visible(true);
                self.chk_send_reply.set_active(true);
                self.comment_view.set_visible(true);
                self.chk_show_free.set_visible(true);
                self.chk_preserve_reminder.set_visible(true);
                self.btn_import_as_event.set_visible(true);
                self.lbl_import_hint.set_visible(true);
            }
            ItipMethod::Cancel => {
                self.icon.set_icon_name(Some("process-stop-symbolic"));
                self.icon.add_css_class("error");
                self.lbl_method.set_text(&gettext("Meeting has been cancelled"));
                let cancel_text = if !org_name.is_empty() {
                    gettext("{} has cancelled this meeting").replace("{}", &org_name)
                } else {
                    gettext("Meeting has been cancelled")
                };
                self.lbl_header_prompt.set_text(&cancel_text);
                self.lbl_cancel_inline.set_text(&cancel_text);
                self.lbl_cancel_inline.set_tooltip_text(Some(&cancel_text));
                self.banner_box.add_css_class("itip-cancelled");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(true);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
                self.secondary_bar_box.set_visible(false);
                self.btn_target_calendar.set_visible(false);
                self.btn_toggle_comment.set_visible(false);
                self.sep_footer_inner.set_visible(false);
                self.btn_toggle_comment.set_active(false);
                self.comment_revealer.set_reveal_child(false);
                self.chk_send_reply.set_visible(false);
                self.comment_view.set_visible(false);
                self.chk_show_free.set_visible(false);
                self.chk_preserve_reminder.set_visible(false);
                self.btn_import_as_event.set_visible(false);
                self.lbl_import_hint.set_visible(false);
            }
            ItipMethod::Reply => {
                self.icon.set_icon_name(Some("mail-reply-sender-symbolic"));
                self.icon.add_css_class("accent");
                self.lbl_method.set_text(&gettext("Response to Meeting Invitation"));
                self.lbl_header_prompt.set_text(&gettext("Response to Meeting Invitation"));
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(true);
                self.action_box_publish.set_visible(false);
                self.secondary_bar_box.set_visible(false);
                self.btn_target_calendar.set_visible(false);
                self.btn_toggle_comment.set_visible(false);
                self.sep_footer_inner.set_visible(false);
                self.chk_send_reply.set_visible(false);
                self.comment_view.set_visible(false);
                self.chk_show_free.set_visible(false);
                self.chk_preserve_reminder.set_visible(false);
                self.btn_import_as_event.set_visible(false);
                self.lbl_import_hint.set_visible(false);
            }
            ItipMethod::Publish => {
                self.icon.set_icon_name(Some("x-office-calendar-symbolic"));
                self.icon.add_css_class("accent");
                self.lbl_method.set_text(&gettext("Published Event"));
                self.lbl_header_prompt.set_text(&gettext("Published Event"));
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(true);
                self.secondary_bar_box.set_visible(true);
                self.btn_target_calendar.set_visible(true);
                self.btn_toggle_comment.set_visible(false);
                self.sep_footer_inner.set_visible(false);
                self.chk_send_reply.set_visible(false);
                self.comment_view.set_visible(false);
                self.chk_show_free.set_visible(false);
                self.chk_preserve_reminder.set_visible(false);
                self.btn_import_as_event.set_visible(false);
                self.lbl_import_hint.set_visible(false);
            }
            ItipMethod::Unknown => {
                self.icon.set_icon_name(Some("x-office-calendar-symbolic"));
                self.icon.add_css_class("accent");
                self.lbl_method.set_text(&gettext("Calendar Entry"));
                self.lbl_header_prompt.set_text(&gettext("Calendar Entry"));
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
                self.secondary_bar_box.set_visible(false);
                self.btn_target_calendar.set_visible(false);
                self.btn_toggle_comment.set_visible(false);
                self.sep_footer_inner.set_visible(false);
                self.chk_send_reply.set_visible(false);
                self.comment_view.set_visible(false);
                self.chk_show_free.set_visible(false);
                self.chk_preserve_reminder.set_visible(false);
                self.btn_import_as_event.set_visible(false);
                self.lbl_import_hint.set_visible(false);
            }
        }

        *self.full_header_prompt.borrow_mut() = self.lbl_header_prompt.text().to_string();
        self.details_scrolled.vadjustment().set_value(0.0);
        if !matches!(invitation.method, ItipMethod::Unknown) {
            self.header_row.set_visible(false);
            self.footer_box.set_margin_top(8);
            self.footer_box.set_margin_bottom(8);
        } else {
            self.header_row.set_visible(true);
            self.header_row.set_margin_top(10);
            self.header_row.set_margin_bottom(6);
            self.footer_box.set_margin_top(0);
            self.footer_box.set_margin_bottom(10);
        }
        self.set_compact(false);

        self.comment_view.buffer().set_text("");

        // Time / Date range formatted in user's locale
        let time_str = format_itip_event_time_range(event);
        self.lbl_time.set_text(&time_str);

        // Calendar conflict check, Day Timeline, and Smart Target Calendar Selection
        let initial_snapshot = load_local_calendar_snapshot_safe();
        self.update_calendar_presence_status(event, &initial_snapshot);
        self.apply_smart_calendar_selection(&initial_snapshot);

        if matches!(invitation.method, ItipMethod::Request | ItipMethod::Add) {
            self.update_conflicts_and_timeline(event, &initial_snapshot);
            self.conflict_row.set_visible(true);

            // Background refresh from EDS to ensure live server data without blocking UI thread
            let (tx, rx) = async_channel::bounded(1);
            gio::spawn_blocking(move || {
                let result = std::panic::catch_unwind(|| {
                    crate::services::ecal_service::load_snapshot_prefer_eds()
                });
                let _ = tx.send_blocking(result);
            });

            let this = self.clone();
            let target_uid = event.uid.clone();
            let target_start = event.start.clone();
            glib::spawn_future_local(async move {
                if let Ok(Ok(load_res)) = rx.recv().await {
                    let current = this.current_invitation.borrow();
                    if let Some(ref cur) = *current {
                        if cur.event.uid == target_uid && cur.event.start == target_start {
                            this.update_conflicts_and_timeline(&cur.event, &load_res.snapshot);
                            this.update_calendar_presence_status(&cur.event, &load_res.snapshot);
                            drop(current);
                            this.apply_smart_calendar_selection(&load_res.snapshot);
                        }
                    }
                }
            });
        } else {
            self.conflict_row.set_visible(false);
            self.render_day_timeline(event, &initial_snapshot, false, &[]);
        }

        // Recurrence
        if let Some(rrule) = &invitation.recurrence {
            self.lbl_recurrence.set_text(&format_recurrence_summary(rrule));
            self.recurrence_row.set_visible(true);
        } else {
            self.recurrence_row.set_visible(false);
        }

        // Location
        self.location_map_popover.popdown();
        let loc = event.location.trim();
        let geo = extract_ical_geo(&event.component);
        self.current_location_geo.set(geo);
        if !loc.is_empty() {
            *self.current_location_text.borrow_mut() = loc.to_string();
            self.location_map_lbl.set_text(loc);
            if loc.starts_with("http://") || loc.starts_with("https://") {
                self.lbl_location.set_markup(&format!(
                    "<a href=\"{}\">{}</a>",
                    glib::markup_escape_text(loc),
                    glib::markup_escape_text(loc)
                ));
                self.location_row.remove_css_class("itip-location-interactive");
                self.location_row.set_cursor_from_name(None);
            } else {
                self.lbl_location.set_text(loc);
                let map_interactive = AppSettings::try_new()
                    .map(|s| s.calendar_location_map_hover() || s.calendar_location_open_map())
                    .unwrap_or(true);
                if map_interactive {
                    self.location_row.add_css_class("itip-location-interactive");
                } else {
                    self.location_row.remove_css_class("itip-location-interactive");
                }
                let open_enabled = AppSettings::try_new()
                    .map(|s| s.calendar_location_open_map())
                    .unwrap_or(true);
                self.location_map_open_btn.set_visible(open_enabled);
                self.location_row.set_cursor_from_name(if open_enabled {
                    Some("pointer")
                } else {
                    None
                });
            }
            self.location_row.set_visible(true);
        } else {
            self.current_location_text.borrow_mut().clear();
            self.location_map_lbl.set_text("");
            self.location_row.set_visible(false);
        }

        // Description - directly visible in sender_message_box
        let desc = event.description.trim();
        if !desc.is_empty() {
            self.lbl_description.set_text(desc);
            self.lbl_sender_message.set_text(desc);
            self.sender_message_box.set_visible(true);
        } else {
            self.lbl_description.set_text("");
            self.lbl_sender_message.set_text("");
            self.sender_message_box.set_visible(false);
        }
        self.exp_description.set_visible(false);

        // Organizer & Attendees (populated on labels for state/tooltips, hidden in UI because the Mail Header directly above already displays sender & recipients)
        if let Some(person) = &invitation.organizer_person {
            let display_name = person.display_label();
            if !display_name.is_empty() {
                self.lbl_organizer.set_text(&format!("{}: {display_name}", gettext("Organizer")));
                let tooltip = person.full_display();
                self.lbl_organizer.set_tooltip_text(Some(&tooltip));
                self.organizer_row.set_tooltip_text(Some(&tooltip));
            }
        } else if !invitation.organizer.trim().is_empty() {
            let org = crate::services::itip_service::strip_mailto_prefix(invitation.organizer.trim());
            self.lbl_organizer.set_text(&format!("{}: {org}", gettext("Organizer")));
            self.lbl_organizer.set_tooltip_text(Some(org));
            self.organizer_row.set_tooltip_text(Some(org));
        }
        self.organizer_row.set_visible(false);

        let attendees = &invitation.attendee_persons;
        if !attendees.is_empty() {
            let count = attendees.len();
            let names = attendees
                .iter()
                .map(|p| p.display_label())
                .collect::<Vec<_>>()
                .join(", ");
            let label_text = if count == 1 {
                format!("{}: {names}", gettext("Attendee"))
            } else if count <= 3 {
                format!("{}: {names}", gettext("Attendees"))
            } else {
                let base = gettext("{} attendees").replace("{}", &count.to_string());
                format!("{base} ({names})")
            };

            let tooltip_lines = attendees
                .iter()
                .map(|p| p.full_display())
                .collect::<Vec<_>>()
                .join("\n");

            self.lbl_attendees.set_text(&label_text);
            self.lbl_attendees.set_tooltip_text(Some(&tooltip_lines));
            self.attendees_row.set_tooltip_text(Some(&tooltip_lines));
        } else if !invitation.attendees.is_empty() {
            let count = invitation.attendees.len();
            let text = if count == 1 {
                gettext("1 attendee")
            } else {
                gettext("{} attendees").replace("{}", &count.to_string())
            };
            let tooltip_lines = invitation
                .attendees
                .iter()
                .map(|s| crate::services::itip_service::strip_mailto_prefix(s))
                .collect::<Vec<_>>()
                .join("\n");
            self.lbl_attendees.set_text(&text);
            self.lbl_attendees.set_tooltip_text(Some(&tooltip_lines));
            self.attendees_row.set_tooltip_text(Some(&tooltip_lines));
        }
        self.attendees_row.set_visible(false);

        // Current user status
        if matches!(invitation.method, ItipMethod::Request | ItipMethod::Add) {
            let user_status = find_user_status(&invitation.raw_component, user_emails);
            self.update_status_badge(user_status);
            self.lbl_status_badge.set_visible(true);
        } else {
            self.lbl_status_badge.set_visible(false);
        }

        self.btn_accept.set_sensitive(true);
        self.btn_tentative.set_sensitive(true);
        self.btn_decline.set_sensitive(true);
        self.btn_remove.set_sensitive(true);
        self.btn_update.set_sensitive(true);
        self.btn_import.set_sensitive(true);

        self.revealer.set_visible(true);
        self.revealer.set_reveal_child(true);
    }

    fn update_status_badge(&self, status: Option<ItipReplyStatus>) {
        self.lbl_status_badge.remove_css_class("accepted");
        self.lbl_status_badge.remove_css_class("tentative");
        self.lbl_status_badge.remove_css_class("declined");
        self.lbl_status_badge.remove_css_class("pending");

        self.icon.remove_css_class("accent");
        self.icon.remove_css_class("error");
        self.icon.remove_css_class("warning");
        self.icon.remove_css_class("success");

        match status {
            Some(ItipReplyStatus::Accepted) => {
                self.lbl_status_badge.set_text(&gettext("Accepted ✓"));
                self.lbl_status_badge.add_css_class("accepted");
                self.banner_box.add_css_class("itip-accepted");
                self.banner_box.remove_css_class("itip-cancelled");
                self.icon.add_css_class("success");
            }
            Some(ItipReplyStatus::Tentative) => {
                self.lbl_status_badge.set_text(&gettext("Tentative"));
                self.lbl_status_badge.add_css_class("tentative");
                self.icon.add_css_class("warning");
            }
            Some(ItipReplyStatus::Declined) => {
                self.lbl_status_badge.set_text(&gettext("Declined"));
                self.lbl_status_badge.add_css_class("declined");
                self.banner_box.add_css_class("itip-cancelled");
                self.banner_box.remove_css_class("itip-accepted");
                self.icon.add_css_class("error");
            }
            None => {
                self.lbl_status_badge.set_text(&gettext("Response Pending"));
                self.lbl_status_badge.add_css_class("pending");
                self.icon.add_css_class("accent");
            }
        }
    }

    fn connect_signals(&self) {
        let this = self.clone();
        self.btn_accept.connect_clicked(move |_| {
            this.handle_response_action(ItipReplyStatus::Accepted);
        });

        let this = self.clone();
        self.btn_tentative.connect_clicked(move |_| {
            this.handle_response_action(ItipReplyStatus::Tentative);
        });

        let this = self.clone();
        self.btn_decline.connect_clicked(move |_| {
            this.handle_response_action(ItipReplyStatus::Declined);
        });

        let this = self.clone();
        self.btn_remove.connect_clicked(move |_| {
            this.handle_remove_action();
        });

        let this = self.clone();
        self.btn_update.connect_clicked(move |_| {
            this.handle_update_action();
        });

        let this = self.clone();
        self.btn_import.connect_clicked(move |_| {
            this.handle_import_action();
        });

        let this = self.clone();
        self.btn_import_as_event.connect_clicked(move |_| {
            this.handle_import_as_event();
        });

        let this = self.clone();
        self.btn_open_calendar.connect_clicked(move |_| {
            this.handle_open_calendar();
        });
    }

    fn handle_response_action(&self, status: ItipReplyStatus) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        self.btn_accept.set_sensitive(false);
        self.btn_tentative.set_sensitive(false);
        self.btn_decline.set_sensitive(false);

        let source_uid = self.selected_source_uid();
        let invitation_for_thread = invitation.clone();
        let status_label = status.as_str().to_string();
        let send_reply = self.chk_send_reply.is_active();
        let comment = self.comment_text();
        let show_free = self.chk_show_free.is_active();

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let service = ECalService::new();
            let mut component = invitation_for_thread.event.component.clone();
            // Update attendee status in the component
            let user_email = invitation_for_thread.organizer.clone(); // fallback
            component = update_attendee_partstat(&component, &user_email, status);
            if show_free {
                component = set_component_transparency(&component, true);
            }

            let res = if status == ItipReplyStatus::Declined {
                // If declined, try to remove or modify
                let _ = service.remove_component(&source_uid, ECalComponentType::Event, &invitation_for_thread.event.uid);
                Ok(())
            } else {
                match service.create_component(&source_uid, ECalComponentType::Event, &component) {
                    Ok(_) => Ok(()),
                    Err(_) => {
                        // If already exists, try modify
                        service
                            .modify_component(
                                &source_uid,
                                ECalComponentType::Event,
                                &invitation_for_thread.event.uid,
                                &component,
                            )
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    }
                }
            };
            let _ = sender.send_blocking(res);
        });

        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(result) = receiver.recv().await {
                match result {
                    Ok(()) => {
                        this.update_status_badge(Some(status));
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            let msg = if send_reply {
                                format!("{}: {status_label}", gettext("Event updated and calendar refreshed"))
                            } else {
                                gettext("Calendar updated (no email reply sent)")
                            };
                            toast(msg);
                        }
                        if let Some(draft_cb) = this.on_reply_draft_saved.borrow().as_ref() {
                            draft_cb(&invitation, status, comment, send_reply);
                        }
                    }
                    Err(err) => {
                        this.btn_accept.set_sensitive(true);
                        this.btn_tentative.set_sensitive(true);
                        this.btn_decline.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("{}: {err}", gettext("Error while updating")));
                        }
                    }
                }
            }
        });
    }

    fn handle_import_as_event(&self) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        self.btn_import_as_event.set_sensitive(false);
        let source_uid = self.selected_source_uid();
        let mut component = invitation.event.component.clone();
        component = strip_organizer_and_attendees(&component);

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = ECalService::new().create_component(&source_uid, ECalComponentType::Event, &component);
            let _ = sender.send_blocking(res);
        });

        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(result) = receiver.recv().await {
                match result {
                    Ok(_) => {
                        this.btn_import_as_event.set_label(&format!("{} ✓", gettext("Imported")));
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(gettext("Imported event without organizer and attendees"));
                        }
                    }
                    Err(err) => {
                        this.btn_import_as_event.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("{}: {err}", gettext("Could not import event")));
                        }
                    }
                }
            }
        });
    }

    fn handle_remove_action(&self) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        self.btn_remove.set_sensitive(false);
        let source_uid = self.selected_source_uid();
        let uid = invitation.event.uid.clone();

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = ECalService::new().remove_component(&source_uid, ECalComponentType::Event, &uid);
            let _ = sender.send_blocking(res);
        });

        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(result) = receiver.recv().await {
                match result {
                    Ok(_) => {
                        this.btn_remove.set_label(&gettext("Removed from calendar"));
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(gettext("Event removed from calendar"));
                        }
                    }
                    Err(err) => {
                        this.btn_remove.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("{}: {err}", gettext("Could not remove event")));
                        }
                    }
                }
            }
        });
    }

    fn handle_update_action(&self) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        self.btn_update.set_sensitive(false);
        let source_uid = self.selected_source_uid();
        let component = invitation.event.component.clone();
        let uid = invitation.event.uid.clone();

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = ECalService::new().modify_component(&source_uid, ECalComponentType::Event, &uid, &component);
            let _ = sender.send_blocking(res);
        });

        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(result) = receiver.recv().await {
                match result {
                    Ok(_) => {
                        this.btn_update.set_label(&format!("{} ✓", gettext("Updated")));
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(gettext("Calendar event updated successfully"));
                        }
                    }
                    Err(err) => {
                        this.btn_update.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("{}: {err}", gettext("Failed to update event")));
                        }
                    }
                }
            }
        });
    }

    fn handle_import_action(&self) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        self.btn_import.set_sensitive(false);
        let source_uid = self.selected_source_uid();
        let component = invitation.event.component.clone();

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = ECalService::new().create_component(&source_uid, ECalComponentType::Event, &component);
            let _ = sender.send_blocking(res);
        });

        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(result) = receiver.recv().await {
                match result {
                    Ok(_) => {
                        this.btn_import.set_label(&gettext("Added ✓"));
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(gettext("Event added to calendar"));
                        }
                    }
                    Err(err) => {
                        this.btn_import.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("{}: {err}", gettext("Error while adding")));
                        }
                    }
                }
            }
        });
    }

    fn handle_open_calendar(&self) {
        let Some(invitation) = self.current_invitation.borrow().clone() else {
            return;
        };

        // Extract date from event.start
        let date = parse_start_date(&invitation.event.start);
        if let Some(navigate) = self.on_calendar_navigate.borrow().as_ref() {
            navigate(date);
        }
    }
}

fn set_component_transparency(component: &str, transparent: bool) -> String {
    let mut lines = Vec::new();
    let transp_val = if transparent { "TRANSP:TRANSPARENT" } else { "TRANSP:OPAQUE" };
    let mut found = false;
    for line in crate::services::itip_service::unfold_lines(component) {
        if line.starts_with("TRANSP:") || line.starts_with("TRANSP;") {
            lines.push(transp_val.to_string());
            found = true;
        } else if line.eq_ignore_ascii_case("END:VEVENT") && !found {
            lines.push(transp_val.to_string());
            lines.push(line);
            found = true;
        } else {
            lines.push(line);
        }
    }
    lines.join("\r\n")
}

fn strip_organizer_and_attendees(component: &str) -> String {
    let mut lines = Vec::new();
    for line in crate::services::itip_service::unfold_lines(component) {
        let prop = line.split(&[':', ';'][..]).next().unwrap_or("").trim();
        if prop.eq_ignore_ascii_case("ORGANIZER") || prop.eq_ignore_ascii_case("ATTENDEE") {
            continue;
        }
        lines.push(line);
    }
    lines.join("\r\n")
}

impl ItipBanner {

    fn update_conflicts_and_timeline(
        &self,
        event: &crate::services::calendar_store_service::CalendarEventRecord,
        snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
    ) {
        let (has_conflict, conflicts) = check_calendar_conflicts_with_snapshot(event, snapshot);

        if has_conflict {
            self.conflict_icon.set_icon_name(Some("dialog-warning-symbolic"));
            self.conflict_icon.remove_css_class("dim-label");
            self.conflict_icon.remove_css_class("success");
            self.conflict_icon.add_css_class("warning");
            self.lbl_conflict.remove_css_class("dim-label");
            self.lbl_conflict.add_css_class("warning");
            let conflict_msg = if conflicts.len() == 1 {
                format!("{}: {}", gettext("Conflict with"), conflicts[0])
            } else {
                format!(
                    "{} {}",
                    gettext("{} conflicts:").replace("{}", &conflicts.len().to_string()),
                    conflicts.join(", ")
                )
            };
            self.lbl_conflict.set_text(&conflict_msg);
            self.conflict_row.set_tooltip_text(Some(&conflicts.join("\n")));
        } else {
            self.conflict_icon.set_icon_name(Some("object-select-symbolic"));
            self.conflict_icon.remove_css_class("warning");
            self.conflict_icon.add_css_class("dim-label");
            self.lbl_conflict.remove_css_class("warning");
            self.lbl_conflict.add_css_class("dim-label");
            self.lbl_conflict.set_text(&gettext("No scheduling conflicts"));
            self.conflict_row.set_tooltip_text(None);
        }

        self.render_day_timeline(event, snapshot, has_conflict, &conflicts);
    }

    fn render_day_timeline(
        &self,
        target_event: &crate::services::calendar_store_service::CalendarEventRecord,
        snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
        has_conflict: bool,
        conflicts: &[String],
    ) {
        *self.last_timeline_state.borrow_mut() = Some((
            target_event.clone(),
            snapshot.clone(),
            has_conflict,
            conflicts.to_vec(),
        ));
        self.render_day_timeline_internal(target_event, snapshot, has_conflict, conflicts, true);
    }

    fn render_day_timeline_internal(
        &self,
        target_event: &crate::services::calendar_store_service::CalendarEventRecord,
        snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
        has_conflict: bool,
        conflicts: &[String],
        reset_scroll: bool,
    ) {
        let hour_h = self.timeline_hour_height.get();
        self.timeline_placed_widgets.borrow_mut().clear();
        while let Some(child) = self.timeline_all_day_box.first_child() {
            self.timeline_all_day_box.remove(&child);
        }
        while let Some(child) = self.timeline_fixed.first_child() {
            self.timeline_fixed.remove(&child);
        }

        let (target_date, target_start_min) = extract_date_and_minutes(&target_event.start);
        let (_, target_end_min) = extract_date_and_minutes(&target_event.end);
        let target_end_min = target_end_min.or_else(|| target_start_min.map(|s| (s + 60).min(1440)));

        let date_label = format_timeline_date(target_date);
        self.lbl_timeline_date.set_text(&date_label);

        if has_conflict {
            let conflict_count = conflicts.len();
            let badge_text = if conflict_count == 1 {
                format!("1 {}", gettext("Conflict"))
            } else {
                format!("{conflict_count} {}", gettext("Conflicts"))
            };
            self.lbl_timeline_badge.set_label(&badge_text);
            self.lbl_timeline_badge.set_tooltip_text(Some(&conflicts.join("\n")));
            self.lbl_timeline_badge.set_visible(true);
        } else {
            self.lbl_timeline_badge.set_tooltip_text(None);
            self.lbl_timeline_badge.set_visible(false);
        }
        self.timeline_side_box.set_visible(true);

        if reset_scroll {
            let scroll_hour = target_start_min.map(|m| (m / 60) as f64).unwrap_or(8.0);
            let target_scroll_y = ((scroll_hour - 1.0).max(0.0)) * hour_h;
            self.timeline_target_scroll_y.set(target_scroll_y);
            let vadj = self.timeline_scrolled.vadjustment();
            glib::idle_add_local_once(move || {
                vadj.set_value(target_scroll_y);
            });
        }

        let is_dark = adw::StyleManager::default().is_dark();

        // 1. All-day items
        let mut has_all_day = false;
        if target_event.all_day {
            has_all_day = true;
            let title = if target_event.summary.trim().is_empty() {
                gettext("(Untitled)")
            } else {
                target_event.summary.trim().to_string()
            };
            let color_class = ensure_timeline_event_css("#3584e4", true, true, is_dark);
            let chip = gtk::Label::builder()
                .label(&title)
                .xalign(0.0)
                .halign(gtk::Align::Fill)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(vec![
                    "appointment-timeline-event",
                    "appointment-timeline-preview",
                    "itip-timeline-target-all-day",
                    &color_class,
                ])
                .tooltip_text(&format!("{title}\n{}", gettext("All Day (Proposed Event)")))
                .build();
            self.timeline_all_day_box.append(&chip);
        }

        for ev in &snapshot.events {
            if !target_event.uid.is_empty() && ev.uid == target_event.uid {
                continue;
            }
            if crate::views::calendar_page::event_occurs_on(ev, target_date) && ev.all_day {
                has_all_day = true;
                let color = crate::views::calendar_page::event_color_for_record(ev);
                let color_class = ensure_timeline_event_css(&color, false, true, is_dark);
                let title = if ev.summary.trim().is_empty() {
                    gettext("(Untitled)")
                } else {
                    ev.summary.trim().to_string()
                };
                let chip = gtk::Label::builder()
                    .label(&title)
                    .xalign(0.0)
                    .halign(gtk::Align::Fill)
                    .ellipsize(pango::EllipsizeMode::End)
                    .css_classes(vec!["appointment-timeline-event", &color_class])
                    .tooltip_text(&format!("{title}\n{}", gettext("All Day")))
                    .build();
                self.timeline_all_day_box.append(&chip);
            }
        }
        self.timeline_all_day_box.set_visible(has_all_day);

        // 2. Timed items
        struct TimedItem {
            is_target: bool,
            start_min: u32,
            end_min: u32,
            title: String,
            color: String,
            uid: Option<String>,
            location: String,
            description: String,
        }

        let mut timed_items: Vec<TimedItem> = Vec::new();

        for ev in &snapshot.events {
            if !target_event.uid.is_empty() && ev.uid == target_event.uid {
                continue;
            }
            if ev.all_day {
                continue;
            }
            if crate::views::calendar_page::event_occurs_on(ev, target_date) {
                let (ev_start, ev_end, _time_text) =
                    crate::views::calendar_page::event_occurrence_times_for_date(ev, target_date, 30);
                let ev_title = if ev.summary.trim().is_empty() {
                    gettext("(Untitled)")
                } else {
                    ev.summary.trim().to_string()
                };
                let color = crate::views::calendar_page::event_color_for_record(ev);
                timed_items.push(TimedItem {
                    is_target: false,
                    start_min: ev_start,
                    end_min: ev_end.max(ev_start + 15),
                    title: ev_title,
                    color,
                    uid: Some(ev.uid.clone()),
                    location: ev.location.clone(),
                    description: ev.description.clone(),
                });
            }
        }

        if !target_event.all_day {
            if let Some(s_min) = target_start_min {
                let e_min = target_end_min.unwrap_or(s_min + 60).max(s_min + 15);
                let title = if target_event.summary.trim().is_empty() {
                    gettext("Meeting Invitation")
                } else {
                    target_event.summary.trim().to_string()
                };
                timed_items.push(TimedItem {
                    is_target: true,
                    start_min: s_min,
                    end_min: e_min,
                    title,
                    color: "#3584e4".to_string(),
                    uid: Some(target_event.uid.clone()),
                    location: target_event.location.clone(),
                    description: target_event.description.clone(),
                });
            }
        }

        timed_items.sort_by(|a, b| {
            a.start_min
                .cmp(&b.start_min)
                .then_with(|| a.end_min.cmp(&b.end_min))
                .then_with(|| a.is_target.cmp(&b.is_target))
        });

        let mut conflicting_uids = std::collections::HashSet::new();
        if let Some(target) = timed_items.iter().find(|i| i.is_target) {
            for other in &timed_items {
                if !other.is_target {
                    if target.start_min < other.end_min && target.end_min > other.start_min {
                        if let Some(ref uid) = other.uid {
                            conflicting_uids.insert(uid.clone());
                        }
                    }
                }
            }
        }

        let mut clusters: Vec<Vec<TimedItem>> = Vec::new();
        let mut current_cluster: Vec<TimedItem> = Vec::new();
        let mut cluster_end = 0;

        for item in timed_items {
            if current_cluster.is_empty() || item.start_min < cluster_end {
                cluster_end = cluster_end.max(item.end_min);
                current_cluster.push(item);
            } else {
                clusters.push(std::mem::take(&mut current_cluster));
                cluster_end = item.end_min;
                current_cluster.push(item);
            }
        }
        if !current_cluster.is_empty() {
            clusters.push(current_cluster);
        }

        const TIMELINE_EVENTS_X: f64 = 47.0;
        let tl_w = self.timeline_side_box.width();
        let total_width: f64 = if tl_w > 60 {
            (tl_w as f64 - TIMELINE_EVENTS_X - 15.0).max(120.0)
        } else {
            198.0
        };

        for cluster in clusters {
            let mut col_ends: Vec<u32> = Vec::new();
            let mut placed: Vec<(TimedItem, usize)> = Vec::new();

            for item in cluster {
                let mut col_idx = None;
                for (idx, end) in col_ends.iter_mut().enumerate() {
                    if *end <= item.start_min {
                        *end = item.end_min;
                        col_idx = Some(idx);
                        break;
                    }
                }
                let c = col_idx.unwrap_or_else(|| {
                    let idx = col_ends.len();
                    col_ends.push(item.end_min);
                    idx
                });
                placed.push((item, c));
            }

            let total_cols = col_ends.len().max(1);
            let col_gap = 2.0;
            let col_width = if total_cols > 1 {
                ((total_width - (total_cols as f64 - 1.0) * col_gap) / total_cols as f64).max(32.0)
            } else {
                total_width
            };

            for (item, col_idx) in placed {
                let x = TIMELINE_EVENTS_X + col_idx as f64 * (col_width + col_gap);
                let y = (item.start_min as f64 / 60.0) * hour_h;
                let duration = item.end_min.saturating_sub(item.start_min);
                let h = ((duration.max(20) as f64) / 60.0) * hour_h - 2.0;

                let is_conflict = item
                    .uid
                    .as_ref()
                    .map(|u| conflicting_uids.contains(u))
                    .unwrap_or(false)
                    || (item.is_target && has_conflict);

                let time_prefix = format!("{:02}:{:02}", item.start_min / 60, item.start_min % 60);
                let time_end = format!("{:02}:{:02}", item.end_min / 60, item.end_min % 60);
                let max_lines = if h >= 34.0 {
                    (((h - 6.0) / 16.0).floor() as i32).max(1)
                } else {
                    1
                };
                let is_multiline = max_lines > 1;
                let markup_text = if h < 26.0 {
                    format!(
                        "<span size=\"small\"><b>{}</b></span>",
                        glib::markup_escape_text(&item.title)
                    )
                } else {
                    format!("<b>{}</b>", glib::markup_escape_text(&item.title))
                };

                let mut tooltip = format!(
                    "{}\n{} – {} Uhr{}",
                    item.title,
                    time_prefix,
                    time_end,
                    if is_conflict {
                        format!("\n⚠️ {}", gettext("Scheduling conflict"))
                    } else if item.is_target {
                        format!("\n⭐ {}", gettext("Proposed meeting"))
                    } else {
                        String::new()
                    }
                );
                if !item.location.trim().is_empty() {
                    tooltip.push_str(&format!("\n📍 Ort: {}", item.location.trim()));
                }
                if !item.description.trim().is_empty() {
                    let short_desc: String = item.description.trim().chars().take(150).collect();
                    tooltip.push_str(&format!("\n\n📝 Notizen:\n{short_desc}"));
                }

                let mut css_classes = vec!["appointment-timeline-event", "calendar-grid-event"];
                if item.is_target {
                    css_classes.push("appointment-timeline-preview");
                    css_classes.push("itip-timeline-target-event");
                }
                if is_conflict {
                    css_classes.push("appointment-timeline-conflict");
                    css_classes.push("itip-timeline-conflict-event");
                }

                let color_class = ensure_timeline_event_css(&item.color, item.is_target, false, is_dark);
                css_classes.push(&color_class);

                let widget = gtk::Label::builder()
                    .use_markup(true)
                    .label(&markup_text)
                    .wrap(is_multiline)
                    .wrap_mode(pango::WrapMode::WordChar)
                    .ellipsize(pango::EllipsizeMode::End)
                    .single_line_mode(!is_multiline)
                    .lines(max_lines)
                    .xalign(0.0)
                    .yalign(0.0)
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Fill)
                    .width_request(col_width as i32)
                    .height_request(h.max(20.0) as i32)
                    .css_classes(css_classes)
                    .tooltip_text(&tooltip)
                    .build();

                self.timeline_fixed.put(&widget, x, y);
                self.timeline_placed_widgets
                    .borrow_mut()
                    .push(PlacedTimelineWidget {
                        widget,
                        x,
                        col_idx,
                        total_cols,
                        start_min: item.start_min,
                        end_min: item.end_min,
                    });
            }
        }
    }
}

const HOUR_HEIGHT: f64 = 40.0;

pub(crate) fn load_local_calendar_snapshot_safe(
) -> crate::services::calendar_store_service::CalendarSnapshot {
    if let Some(cached) = crate::services::ecal_service::load_calendar_load_cache() {
        if !cached.events.is_empty() || !cached.tasks.is_empty() || !cached.memos.is_empty() {
            return cached;
        }
    }
    crate::services::calendar_store_service::CalendarStoreService::new().load_snapshot()
}

fn check_calendar_conflicts(
    invitation_event: &crate::services::calendar_store_service::CalendarEventRecord,
) -> (bool, Vec<String>) {
    let snapshot = load_local_calendar_snapshot_safe();
    check_calendar_conflicts_with_snapshot(invitation_event, &snapshot)
}

fn check_calendar_conflicts_with_snapshot(
    invitation_event: &crate::services::calendar_store_service::CalendarEventRecord,
    snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
) -> (bool, Vec<String>) {
    let (target_date, target_start_min) = extract_date_and_minutes(&invitation_event.start);
    let (_, target_end_min) = extract_date_and_minutes(&invitation_event.end);
    let target_end_min = target_end_min.or_else(|| target_start_min.map(|s| (s + 60).min(1440)));

    let mut conflicts = Vec::new();

    for ev in &snapshot.events {
        if !invitation_event.uid.is_empty() && ev.uid == invitation_event.uid {
            continue;
        }

        let (ev_date, ev_start_min) = extract_date_and_minutes(&ev.start);
        if ev_date != target_date {
            continue;
        }

        if invitation_event.all_day || ev.all_day {
            conflicts.push(format!("\"{}\" ({})", ev.summary, gettext("All day")));
            continue;
        }

        let (_, ev_end_min) = extract_date_and_minutes(&ev.end);
        let ev_end_min = ev_end_min.or_else(|| ev_start_min.map(|s| (s + 60).min(1440)));

        if let (Some(t_start), Some(t_end), Some(e_start), Some(e_end)) =
            (target_start_min, target_end_min, ev_start_min, ev_end_min)
        {
            if t_start < e_end && t_end > e_start {
                let time_str = format!(
                    "{:02}:{:02} – {:02}:{:02}",
                    e_start / 60,
                    e_start % 60,
                    e_end / 60,
                    e_end % 60
                );
                conflicts.push(format!("\"{}\" ({time_str})", ev.summary));
            }
        }
    }

    let has_conflict = !conflicts.is_empty();
    (has_conflict, conflicts)
}

fn extract_date_and_minutes(raw: &str) -> (NaiveDate, Option<u32>) {
    let date = parse_start_date(raw);
    let trimmed = raw.trim();
    let time_part = if let Some((_, time_str)) = trimmed.split_once('T') {
        time_str
    } else if let Some((_, time_str)) = trimmed.split_once(' ') {
        time_str
    } else {
        ""
    };
    let minutes = if !time_part.is_empty() {
        let clean_time = time_part.trim_end_matches('Z').trim();
        if let Some((h_str, rest)) = clean_time.split_once(':') {
            let m_str = rest.split(':').next().unwrap_or(rest);
            if let (Ok(h), Ok(m)) = (h_str.trim().parse::<u32>(), m_str.trim().parse::<u32>()) {
                if h < 24 && m < 60 {
                    Some(h * 60 + m)
                } else {
                    None
                }
            } else {
                None
            }
        } else if let Some((h_str, rest)) = clean_time.split_once('.') {
            let m_str = rest.split('.').next().unwrap_or(rest);
            if let (Ok(h), Ok(m)) = (h_str.trim().parse::<u32>(), m_str.trim().parse::<u32>()) {
                if h < 24 && m < 60 {
                    Some(h * 60 + m)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            let digits: String = clean_time.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 4 {
                let h: u32 = digits[0..2].parse().unwrap_or(0);
                let m: u32 = digits[2..4].parse().unwrap_or(0);
                if h < 24 && m < 60 {
                    Some(h * 60 + m)
                } else {
                    None
                }
            } else {
                None
            }
        }
    } else {
        None
    };
    (date, minutes)
}

fn parse_start_date(raw: &str) -> NaiveDate {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return chrono::Local::now().date_naive();
    }
    // First token (before space or 'T')
    let first_token = trimmed
        .split(|c: char| c.is_whitespace() || c == 'T')
        .next()
        .unwrap_or(trimmed);

    // Try YYYY-MM-DD
    if let Ok(d) = NaiveDate::parse_from_str(first_token, "%Y-%m-%d") {
        return d;
    }
    // Try DD.MM.YYYY
    if let Ok(d) = NaiveDate::parse_from_str(first_token.trim_end_matches('.'), "%d.%m.%Y") {
        return d;
    }
    // Fallback: extract ASCII digits safely
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        let first_8 = &digits[0..8];
        if let Ok(d) = NaiveDate::parse_from_str(first_8, "%Y%m%d") {
            return d;
        }
        if let Ok(d) = NaiveDate::parse_from_str(first_8, "%d%m%Y") {
            return d;
        }
    }
    chrono::Local::now().date_naive()
}

fn format_date_localized(date: NaiveDate) -> String {
    let fmt = crate::services::AppSettings::try_new()
        .map(|s| s.resolved_date_format())
        .unwrap_or_else(|| "german".to_string());
    match fmt.as_str() {
        "us" => date.format("%m/%d/%Y").to_string(),
        "iso" => date.format("%Y-%m-%d").to_string(),
        _ => date.format("%d.%m.%Y").to_string(),
    }
}

fn format_timeline_date(date: NaiveDate) -> String {
    let weekday = match date.weekday() {
        chrono::Weekday::Mon => gettext("Monday"),
        chrono::Weekday::Tue => gettext("Tuesday"),
        chrono::Weekday::Wed => gettext("Wednesday"),
        chrono::Weekday::Thu => gettext("Thursday"),
        chrono::Weekday::Fri => gettext("Friday"),
        chrono::Weekday::Sat => gettext("Saturday"),
        chrono::Weekday::Sun => gettext("Sunday"),
    };
    format!("{weekday}, {}", format_date_localized(date))
}

fn format_itip_event_time_range(
    event: &crate::services::calendar_store_service::CalendarEventRecord,
) -> String {
    let (start_date, start_min) = extract_date_and_minutes(&event.start);
    let has_end = !event.end.trim().is_empty();
    let (end_date, end_min) = if has_end {
        extract_date_and_minutes(&event.end)
    } else {
        (start_date, None)
    };

    if event.all_day {
        if has_end && end_date > start_date {
            format!(
                "{} – {} ({})",
                format_timeline_date(start_date),
                format_timeline_date(end_date),
                gettext("All day")
            )
        } else {
            format!("{} ({})", format_timeline_date(start_date), gettext("All day"))
        }
    } else if let Some(s_min) = start_min {
        let s_time = format!("{:02}:{:02}", s_min / 60, s_min % 60);
        if let Some(e_min) = end_min {
            let e_time = format!("{:02}:{:02}", e_min / 60, e_min % 60);
            if !has_end || start_date == end_date {
                format!("{}, {} – {}", format_timeline_date(start_date), s_time, e_time)
            } else {
                format!(
                    "{}, {} – {}, {}",
                    format_timeline_date(start_date),
                    s_time,
                    format_timeline_date(end_date),
                    e_time
                )
            }
        } else {
            format!("{}, {}", format_timeline_date(start_date), s_time)
        }
    } else {
        format_timeline_date(start_date)
    }
}

fn ensure_timeline_event_css(color: &str, is_preview: bool, is_all_day: bool, is_dark: bool) -> String {
    let clean = color.trim_start_matches('#').to_lowercase();
    let mode = if is_dark { "dark" } else { "light" };
    let preview_tag = if is_preview { "prev" } else { "norm" };
    let type_tag = if is_all_day { "allday" } else { "timed" };
    let class_name = format!("itip-tl-{clean}-{preview_tag}-{type_tag}-{mode}");

    thread_local! {
        static REGISTERED: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    }

    let already_registered = REGISTERED.with(|r| r.borrow().contains(&class_name));
    if !already_registered {
        if let Some(display) = gtk::gdk::Display::default() {
            let (bg, _, _, fg) = crate::views::calendar_page::pastel_colors_for_hex(color, is_dark);
            let border_rule = if is_preview {
                format!("border: none; border-left: 6px solid {color}; box-shadow: inset 0 0 0 1.5px {color}; font-weight: 600;")
            } else {
                format!("border: none; border-left: 6px solid {color}; box-shadow: none; font-weight: 500;")
            };
            let (radius, padding) = if is_all_day {
                ("6px", "3px 8px")
            } else {
                ("6px", "3px 6px")
            };
            let css = format!(
                "label.{class_name} {{ background-color: {bg}; color: {fg}; {border_rule} border-radius: {radius}; padding: {padding}; transition: filter 120ms ease, box-shadow 120ms ease; }}\n\
                 label.{class_name}:hover {{ filter: brightness(0.96); box-shadow: 0 2px 6px alpha(black, 0.14); }}"
            );
            let p = gtk::CssProvider::new();
            p.load_from_string(&css);
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            REGISTERED.with(|r| r.borrow_mut().insert(class_name.clone()));
        }
    }

    class_name
}

const MIN_TIMELINE_SIDE_WIDTH: i32 = 280;

/// Computes the target width for the right-hand mini-calendar (`timeline_side_box`) in side-by-side mode:
/// - Always at least `280px` (`MIN_TIMELINE_SIDE_WIDTH`).
/// - Scales smoothly with the preview pane width (`pane_w`), expanding further when the left column
///   only has compact metadata (`compact_metadata == true`) or when overlapping events exist (`max_cols > 1`).
fn compute_dynamic_timeline_width(pane_w: i32, compact_metadata: bool, max_cols: usize) -> i32 {
    if pane_w <= 620 {
        return MIN_TIMELINE_SIDE_WIDTH;
    }
    let ratio = if compact_metadata { 0.44 } else { 0.35 };
    let col_bonus = if max_cols > 1 {
        ((max_cols as i32 - 1) * 28).min(64)
    } else {
        0
    };
    let max_allowed = if compact_metadata {
        (pane_w - 310).clamp(MIN_TIMELINE_SIDE_WIDTH, 480)
    } else {
        (pane_w - 370).clamp(MIN_TIMELINE_SIDE_WIDTH, 420)
    };
    ((pane_w as f64 * ratio).round() as i32 + col_bonus).clamp(MIN_TIMELINE_SIDE_WIDTH, max_allowed)
}

fn ensure_itip_base_css() {
    thread_local! {
        static BASE_CSS_LOADED: Cell<bool> = const { Cell::new(false) };
    }
    if BASE_CSS_LOADED.with(|c| c.get()) {
        return;
    }
    if let Some(display) = gtk::gdk::Display::default() {
        let p = gtk::CssProvider::new();
        p.load_from_string(
            "box.itip-location-row { border-radius: 6px; padding: 2px 6px; margin-left: -6px; transition: background-color 120ms ease; }\n\
             box.itip-location-row.itip-location-interactive:hover { background-color: alpha(currentColor, 0.06); }\n\
             box.itip-location-row.itip-location-interactive:hover label.itip-location-label { text-decoration: underline; }\n\
             popover.itip-location-map-popover > contents { padding: 6px; border-radius: 12px; }\n\
             frame.itip-location-map-frame { border-radius: 8px; }\n\
             button.itip-map-action-btn { min-height: 24px; min-width: 24px; padding: 2px; }\n\
             popover.calendar-picker-popover > contents { padding: 3px; border-radius: 12px; box-shadow: 0 4px 16px alpha(black, 0.12); border: 1px solid alpha(@borders, 0.75); }\n\
             menubutton.itip-calendar-chip label { line-height: 1.35; min-height: 20px; padding-top: 1px; padding-bottom: 1px; }\n\
             label.itip-calendar-group-header { font-size: 0.84rem; font-weight: 400; line-height: 1.30; min-height: 18px; color: alpha(@window_fg_color, 0.84); padding: 4px 8px 1px 8px; }\n\
             separator.itip-calendar-group-sep { margin: 2px 8px 1px 8px; opacity: 0.45; }\n\
             button.itip-calendar-chip-row { padding: 1px 8px; border-radius: 7px; margin: 0 2px; min-height: 22px; font-size: 14px; font-weight: 500; color: @window_fg_color; background: transparent; border: none; box-shadow: none; transition: background-color 120ms ease, color 120ms ease; }\n\
             button.itip-calendar-chip-row:hover { background-color: alpha(currentColor, 0.07); }\n\
             button.itip-calendar-chip-row:active { background-color: alpha(currentColor, 0.12); }\n\
             label.itip-calendar-row-label { font-size: 14px; line-height: 1.30; min-height: 20px; }\n\
             label.itip-calendar-row-label-light { color: #000000; font-weight: 500; opacity: 1.0; }\n\
             label.itip-calendar-row-label-dark { color: #ffffff; font-weight: 500; opacity: 1.0; }\n\
             button.itip-calendar-chip-row.is-selected { background-color: alpha(@accent_color, 0.14); color: @accent_color; font-weight: 600; }\n\
             button.itip-calendar-chip-row.is-selected label.itip-calendar-row-label { color: @accent_color; font-weight: 600; }\n\
             box.itip-calendar-color-dot { min-width: 5px; min-height: 16px; border-radius: 2.5px; border: none; padding: 0; margin: 0; }",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &p,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 20,
        );
        BASE_CSS_LOADED.with(|c| c.set(true));
    }
}

fn parse_rgb_hex(hex: &str) -> (u8, u8, u8) {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&clean[0..2], 16),
            u8::from_str_radix(&clean[2..4], 16),
            u8::from_str_radix(&clean[4..6], 16),
        ) {
            return (r, g, b);
        }
    } else if clean.len() == 3 {
        let chars: Vec<char> = clean.chars().collect();
        let r_s = format!("{0}{0}", chars[0]);
        let g_s = format!("{0}{0}", chars[1]);
        let b_s = format!("{0}{0}", chars[2]);
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&r_s, 16),
            u8::from_str_radix(&g_s, 16),
            u8::from_str_radix(&b_s, 16),
        ) {
            return (r, g, b);
        }
    }
    (0x35, 0x84, 0xe4)
}

/// Computes `(fill_hex, border_hex)` for a calendar color stripe so even very pale colors
/// (such as `#e0e0e0`, `#fbe887`, or `#ffff66`) have a rich, legible fill without needing a border.
pub(crate) fn calendar_dot_colors_for_hex(hex: &str, is_dark: bool) -> (String, String) {
    let (r, g, b) = parse_rgb_hex(hex);
    let rf = r as f64;
    let gf = g as f64;
    let bf = b as f64;
    let lum = (0.299 * rf + 0.587 * gf + 0.114 * bf) / 255.0;

    let scale_rgb = |s: f64| -> (u8, u8, u8) {
        (
            (rf * s).round().clamp(0.0, 255.0) as u8,
            (gf * s).round().clamp(0.0, 255.0) as u8,
            (bf * s).round().clamp(0.0, 255.0) as u8,
        )
    };
    let mix_white = |t: f64| -> (u8, u8, u8) {
        (
            (rf + (255.0 - rf) * t).round().clamp(0.0, 255.0) as u8,
            (gf + (255.0 - gf) * t).round().clamp(0.0, 255.0) as u8,
            (bf + (255.0 - bf) * t).round().clamp(0.0, 255.0) as u8,
        )
    };

    let (fill, border) = if !is_dark {
        if lum > 0.72 {
            (scale_rgb(0.82), scale_rgb(0.52))
        } else {
            ((r, g, b), scale_rgb(0.72))
        }
    } else if lum < 0.25 {
        (mix_white(0.28), mix_white(0.55))
    } else if lum > 0.80 {
        ((r, g, b), scale_rgb(0.68))
    } else {
        ((r, g, b), mix_white(0.22))
    };

    (
        format!("#{:02x}{:02x}{:02x}", fill.0, fill.1, fill.2),
        format!("#{:02x}{:02x}{:02x}", border.0, border.1, border.2),
    )
}

fn ensure_calendar_dot_css(color: &str, is_dark: bool) -> String {
    let (fill, _border) = calendar_dot_colors_for_hex(color, is_dark);
    let clean = fill.trim_start_matches('#').to_lowercase();
    let mode = if is_dark { "dark" } else { "light" };
    let class_name = format!("itip-cal-dot-{clean}-{mode}");

    thread_local! {
        static DOT_REGISTERED: RefCell<std::collections::HashSet<String>> = RefCell::new(std::collections::HashSet::new());
    }

    let already = DOT_REGISTERED.with(|r| r.borrow().contains(&class_name));
    if !already {
        if let Some(display) = gtk::gdk::Display::default() {
            let css = format!(
                "box.{class_name} {{ background-color: {fill}; border: none; }}"
            );
            let p = gtk::CssProvider::new();
            p.load_from_string(&css);
            gtk::style_context_add_provider_for_display(
                &display,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            DOT_REGISTERED.with(|r| r.borrow_mut().insert(class_name.clone()));
        }
    }

    class_name
}

fn apply_calendar_dot_style(widget: &impl IsA<gtk::Widget>, color: &str) {
    let is_dark = adw::StyleManager::default().is_dark();
    let new_cls = ensure_calendar_dot_css(color, is_dark);
    let existing: Vec<String> = widget
        .css_classes()
        .into_iter()
        .map(|s| s.to_string())
        .filter(|s| s.starts_with("itip-cal-dot-"))
        .collect();
    for old in existing {
        if old != new_cls {
            widget.remove_css_class(&old);
        }
    }
    if !widget.has_css_class(&new_cls) {
        widget.add_css_class(&new_cls);
    }
}

fn is_primary_calendar_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "kalender" | "calendar" | "personal" | "persönlich" | "personal calendar" | "persönlicher kalender"
    )
}

pub(crate) fn group_and_sort_calendar_sources(
    mut raw_items: Vec<(String, String, Option<String>)>,
) -> Vec<CalendarPickerItem> {
    let local_group = gettext("On This Computer");

    if !raw_items
        .iter()
        .any(|(uid, _, _)| uid == "local-calendar" || uid == "system-calendar")
    {
        raw_items.push((
            "local-calendar".to_string(),
            gettext("Personal Calendar"),
            None,
        ));
    }

    let mut deduped: Vec<(String, String, String)> = Vec::new();
    for (uid, raw_name, parent_opt) in raw_items {
        if deduped.iter().any(|(u, _, _)| u == &uid) {
            continue;
        }
        let group_name = if let Some(p) = parent_opt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            p.to_string()
        } else if let Some(open_idx) = raw_name.rfind(" (") {
            if raw_name.ends_with(')') && open_idx + 2 < raw_name.len() - 1 {
                let candidate = raw_name[open_idx + 2..raw_name.len() - 1].trim();
                if !candidate.is_empty() {
                    candidate.to_string()
                } else {
                    local_group.clone()
                }
            } else {
                local_group.clone()
            }
        } else {
            local_group.clone()
        };

        let clean_name =
            crate::dialogs::event_dialog::clean_calendar_display_name(&raw_name, &group_name);
        deduped.push((uid, group_name, clean_name));
    }

    let mut name_counts = std::collections::HashMap::<String, usize>::new();
    let mut group_order: Vec<String> = Vec::new();
    for (_, group_name, clean_name) in &deduped {
        *name_counts.entry(clean_name.to_lowercase()).or_insert(0) += 1;
        if !group_order.contains(group_name) {
            group_order.push(group_name.clone());
        }
    }

    // Place real account groups first and "On This Computer" at the bottom when other groups exist
    if group_order.len() > 1 {
        group_order.sort_by_key(|g| if g == &local_group { 1usize } else { 0usize });
    }

    let mut result: Vec<CalendarPickerItem> = Vec::new();
    for grp in &group_order {
        let mut grp_items: Vec<CalendarPickerItem> = deduped
            .iter()
            .filter(|(_, g, _)| g == grp)
            .map(|(uid, group_name, clean_name)| {
                let count = name_counts
                    .get(&clean_name.to_lowercase())
                    .copied()
                    .unwrap_or(0);
                let picker_label = if group_name != &local_group
                    && (count > 1 || is_primary_calendar_name(clean_name))
                {
                    if !clean_name
                        .to_lowercase()
                        .contains(&group_name.to_lowercase())
                    {
                        format!("{clean_name} ({group_name})")
                    } else {
                        clean_name.clone()
                    }
                } else {
                    clean_name.clone()
                };
                CalendarPickerItem {
                    uid: uid.clone(),
                    group_name: group_name.clone(),
                    clean_name: clean_name.clone(),
                    picker_label,
                }
            })
            .collect();

        grp_items.sort_by(|a, b| {
            let a_prim = is_primary_calendar_name(&a.clean_name);
            let b_prim = is_primary_calendar_name(&b.clean_name);
            b_prim
                .cmp(&a_prim)
                .then_with(|| a.clean_name.to_lowercase().cmp(&b.clean_name.to_lowercase()))
        });

        result.extend(grp_items);
    }

    result
}

/// Extracts `GEO:lat;lon` (or `GEO:lat,lon`) from the top-level `VEVENT` in an iCalendar component.
pub(crate) fn extract_ical_geo(component: &str) -> Option<(f64, f64)> {
    let mut in_vevent = false;
    let mut nested_depth: usize = 0;
    for line in crate::services::itip_service::unfold_lines(component) {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_vevent = true;
            nested_depth = 0;
            continue;
        }
        if !in_vevent {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            break;
        }
        if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("BEGIN:") {
            nested_depth += 1;
            continue;
        }
        if trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("END:") {
            nested_depth = nested_depth.saturating_sub(1);
            continue;
        }
        if nested_depth > 0 {
            continue;
        }
        let prop = trimmed.split(&[':', ';'][..]).next().unwrap_or("").trim();
        if prop.eq_ignore_ascii_case("GEO") {
            if let Some((_, val)) = trimmed.split_once(':') {
                let parts: Vec<&str> = val.trim().split(&[';', ','][..]).collect();
                if parts.len() == 2 {
                    if let (Ok(lat), Ok(lon)) = (
                        parts[0].trim().parse::<f64>(),
                        parts[1].trim().parse::<f64>(),
                    ) {
                        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
                            return Some((lat, lon));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Generates progressive search queries for a calendar location string so internal room names
/// (e.g. `"Konferenzraum Wiesbaden P&I"`) gracefully fall back to the city/place (`"Wiesbaden"`).
pub(crate) fn smart_location_queries(location: &str) -> Vec<String> {
    let raw = location.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = vec![raw.to_string()];

    const ROOM_PREFIXES: &[&str] = &[
        "konferenzraum",
        "besprechungsraum",
        "meetingraum",
        "meeting room",
        "conference room",
        "sitzungszimmer",
        "schulungsraum",
        "raum",
        "room",
        "gebäude",
        "building",
        "standort",
        "büro",
        "office",
    ];

    let lower = raw.to_lowercase();
    let mut stripped = raw;
    for prefix in ROOM_PREFIXES {
        if lower.starts_with(prefix) {
            let rest = &raw[prefix.len()..];
            if rest.is_empty()
                || rest.starts_with(char::is_whitespace)
                || rest.starts_with(':')
                || rest.starts_with('-')
            {
                stripped = rest.trim_start_matches(|c: char| {
                    c.is_whitespace() || c == ':' || c == '-' || c == ','
                });
                break;
            }
        }
    }

    if !stripped.is_empty() && stripped != raw {
        out.push(stripped.to_string());
    }

    // If stripped still has multiple words or separators (e.g. "Wiesbaden P&I" or "Wiesbaden - 3. OG"),
    // add the primary place token as a fallback query.
    let base = if !stripped.is_empty() { stripped } else { raw };
    if let Some(first_seg) = base
        .split(&['-', '/', '(', ',', '|'][..])
        .next()
        .map(str::trim)
    {
        if !first_seg.is_empty() && !out.iter().any(|q| q.eq_ignore_ascii_case(first_seg)) {
            out.push(first_seg.to_string());
        }
        if let Some(first_word) = first_seg.split_whitespace().next() {
            let clean_word = first_word.trim_matches(|c: char| !c.is_alphanumeric());
            if clean_word.chars().count() >= 3
                && clean_word.chars().any(char::is_alphabetic)
                && !out.iter().any(|q| q.eq_ignore_ascii_case(clean_word))
            {
                out.push(clean_word.to_string());
            }
        }
    }

    out
}

fn build_minimal_map_hover_html(location: &str, geo: Option<(f64, f64)>, is_dark: bool) -> String {
    let bg = if is_dark { "#242424" } else { "#f6f5f4" };
    let fg = if is_dark { "#deddda" } else { "#3d3846" };
    let sub = if is_dark { "#9a9996" } else { "#77767b" };
    let map_filter = if is_dark {
        "filter: invert(0.88) hue-rotate(180deg) brightness(0.95) contrast(0.92);"
    } else {
        ""
    };
    let escaped_loc = glib::markup_escape_text(location);

    if let Some((lat, lon)) = geo {
        let min_lon = lon - 0.009;
        let max_lon = lon + 0.009;
        let min_lat = lat - 0.005;
        let max_lat = lat + 0.005;
        return format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
            html,body{{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:{bg};font-family:sans-serif;}}\
            iframe{{width:100%;height:100%;border:0;{map_filter}}}\
            </style></head><body>\
            <iframe src=\"https://www.openstreetmap.org/export/embed.html?bbox={min_lon},{min_lat},{max_lon},{max_lat}&amp;layer=mapnik&amp;marker={lat},{lon}\"></iframe>\
            </body></html>"
        );
    }

    let queries = smart_location_queries(location);
    let queries_json = serde_json::to_string(&queries).unwrap_or_else(|_| "[]".to_string());

    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
        html,body{{margin:0;padding:0;width:100%;height:100%;overflow:hidden;background:{bg};color:{fg};font-family:system-ui,sans-serif;}}\
        #fallback{{display:flex;flex-direction:column;align-items:center;justify-content:center;width:100%;height:100%;box-sizing:border-box;padding:12px;text-align:center;gap:4px;}}\
        .pin{{font-size:20px;opacity:0.75;}}\
        .title{{font-size:12px;font-weight:600;max-width:230px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}}\
        .hint{{font-size:11px;color:{sub};}}\
        iframe{{width:100%;height:100%;border:0;display:none;{map_filter}}}\
        </style></head><body>\
        <div id=\"fallback\"><div class=\"pin\">⌖</div><div class=\"title\">{escaped_loc}</div><div class=\"hint\" id=\"status\">Locating…</div></div>\
        <iframe id=\"mapframe\"></iframe>\
        <script>\
        (async function(){{\
          const queries = {queries_json};\
          for (const q of queries) {{\
            try {{\
              const r = await fetch('https://nominatim.openstreetmap.org/search?format=json&limit=1&q=' + encodeURIComponent(q), {{headers:{{'Accept-Language':navigator.language||'de,en'}}}});\
              if (!r.ok) continue;\
              const data = await r.json();\
              if (data && data.length > 0) {{\
                const lat = parseFloat(data[0].lat);\
                const lon = parseFloat(data[0].lon);\
                if (!isNaN(lat) && !isNaN(lon)) {{\
                  const dLon = 0.012, dLat = 0.007;\
                  const bbox = [lon - dLon, lat - dLat, lon + dLon, lat + dLat].join(',');\
                  const f = document.getElementById('mapframe');\
                  f.src = 'https://www.openstreetmap.org/export/embed.html?bbox=' + encodeURIComponent(bbox) + '&layer=mapnik&marker=' + encodeURIComponent(lat + ',' + lon);\
                  f.onload = function(){{ document.getElementById('fallback').style.display='none'; f.style.display='block'; }};\
                  return;\
                }}\
              }}\
            }} catch (_) {{}}\
          }}\
          const st = document.getElementById('status');\
          if (st) st.textContent = '';\
        }})();\
        </script></body></html>"
    )
}

fn open_calendar_location_in_map(location: &str, geo: Option<(f64, f64)>) {
    let target = AppSettings::try_new()
        .map(|s| s.contact_open_map_target())
        .unwrap_or_else(|| "openstreetmap".to_string());
    let prefer_local = AppSettings::try_new()
        .map(|s| s.contact_open_map_prefer_local())
        .unwrap_or(true);

    let (primary, fallback) = if let Some((lat, lon)) = geo {
        let web_url = if target.trim().eq_ignore_ascii_case("google") {
            format!("https://www.google.com/maps/search/?api=1&query={lat},{lon}")
        } else {
            format!("https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=16/{lat}/{lon}")
        };
        if prefer_local {
            (format!("geo:{lat},{lon}"), Some(web_url))
        } else {
            (web_url, None)
        }
    } else {
        let queries = smart_location_queries(location);
        let best_query = queries
            .get(1)
            .or_else(|| queries.first())
            .map(String::as_str)
            .unwrap_or(location);
        crate::services::settings_service::build_contact_map_uri(best_query, &target, prefer_local)
    };

    if gio::AppInfo::launch_default_for_uri(&primary, None::<&gio::AppLaunchContext>).is_err() {
        if let Some(fb) = fallback {
            let _ = gio::AppInfo::launch_default_for_uri(&fb, None::<&gio::AppLaunchContext>);
        }
    }
}

/// Smart heuristic for selecting the best target calendar source index:
/// 1. Existing Event UID match in `CalendarSnapshot`: select the calendar where the event already lives.
/// 2. Invited User / Account Email match: match the invited email address (or user account email)
///    against writable calendar sources by full email, provider keywords (`google`/`gmail`, `icloud`,
///    `outlook`/`microsoft`/`exchange`), custom domain stem, or local-part.
/// 3. Explicit `invitation.event.source_uid` match (when not `"local-calendar"`).
/// 4. Fallback to `0` (`Personal Calendar`).
fn find_best_calendar_source_index(
    sources: &[(String, String)],
    invitation: &ItipInvitation,
    user_emails: &[&str],
    snapshot: &crate::services::calendar_store_service::CalendarSnapshot,
) -> Option<usize> {
    if sources.is_empty() {
        return None;
    }

    // 1. Existing Event UID in snapshot -> choose the calendar where it already lives
    let ev_uid = invitation.event.uid.trim();
    if !ev_uid.is_empty() {
        if let Some(existing) = snapshot.events.iter().find(|e| e.uid.trim() == ev_uid) {
            if let Some(idx) = sources.iter().position(|(uid, name)| {
                (!existing.source_uid.is_empty() && uid == &existing.source_uid)
                    || (!existing.source_display_name.is_empty()
                        && name.eq_ignore_ascii_case(&existing.source_display_name))
            }) {
                return Some(idx);
            }
        }
    }

    // Collect candidate emails in priority order:
    // a) user_emails that explicitly appear in the invitation's attendee list
    // b) invitation attendees
    // c) all configured user_emails
    let mut candidate_emails: Vec<String> = Vec::new();
    let attendee_emails_lower: Vec<String> = invitation
        .attendee_persons
        .iter()
        .map(|p| p.email.trim().to_ascii_lowercase())
        .chain(invitation.attendees.iter().map(|a| {
            crate::services::itip_service::strip_mailto_prefix(a.trim()).to_ascii_lowercase()
        }))
        .filter(|e| !e.is_empty())
        .collect();

    for ue in user_emails {
        let clean = crate::services::itip_service::strip_mailto_prefix(ue.trim()).to_ascii_lowercase();
        if !clean.is_empty() && attendee_emails_lower.iter().any(|ae| ae == &clean) {
            if !candidate_emails.contains(&clean) {
                candidate_emails.push(clean);
            }
        }
    }
    for ae in &attendee_emails_lower {
        if !candidate_emails.contains(ae) {
            candidate_emails.push(ae.clone());
        }
    }
    for ue in user_emails {
        let clean = crate::services::itip_service::strip_mailto_prefix(ue.trim()).to_ascii_lowercase();
        if !clean.is_empty() && !candidate_emails.contains(&clean) {
            candidate_emails.push(clean);
        }
    }

    // 2. Match candidate emails against calendar sources
    for email in &candidate_emails {
        // 2a. Exact/substring match of full email in source_uid or display_name
        if let Some(idx) = sources.iter().position(|(uid, name)| {
            uid.to_ascii_lowercase().contains(email) || name.to_ascii_lowercase().contains(email)
        }) {
            return Some(idx);
        }

        if let Some((local_part, domain)) = email.split_once('@') {
            let domain_stem = domain.split('.').next().unwrap_or(domain);

            // 2b. Well-known provider keyword matching
            let is_google = domain == "gmail.com" || domain == "googlemail.com";
            let is_icloud = domain == "icloud.com" || domain == "me.com" || domain == "mac.com";
            let is_microsoft = domain == "outlook.com"
                || domain == "hotmail.com"
                || domain == "live.com"
                || domain.ends_with(".onmicrosoft.com");

            if let Some(idx) = sources.iter().position(|(uid, name)| {
                let hay = format!("{} {}", uid.to_ascii_lowercase(), name.to_ascii_lowercase());
                (is_google && (hay.contains("google") || hay.contains("gmail")))
                    || (is_icloud && hay.contains("icloud"))
                    || (is_microsoft
                        && (hay.contains("outlook")
                            || hay.contains("microsoft")
                            || hay.contains("exchange")
                            || hay.contains("office365")))
            }) {
                return Some(idx);
            }

            // 2c. Custom / work domain stem match (e.g. "corpdom" or "acme" matching "ACME Work Calendar")
            if domain_stem.len() >= 3
                && !is_google
                && !is_icloud
                && !is_microsoft
            {
                if let Some(idx) = sources.iter().position(|(uid, name)| {
                    uid.to_ascii_lowercase().contains(domain)
                        || name.to_ascii_lowercase().contains(domain)
                        || uid.to_ascii_lowercase().contains(domain_stem)
                        || name.to_ascii_lowercase().contains(domain_stem)
                }) {
                    return Some(idx);
                }
            }

            // 2d. Local-part match (if specific enough, >= 4 chars)
            if local_part.len() >= 4 {
                if let Some(idx) = sources.iter().position(|(uid, name)| {
                    uid != "local-calendar"
                        && (uid.to_ascii_lowercase().contains(local_part)
                            || name.to_ascii_lowercase().contains(local_part))
                }) {
                    return Some(idx);
                }
            }
        }
    }

    // 3. Explicit source_uid on invitation event (if not default "local-calendar")
    let inv_source = invitation.event.source_uid.trim();
    if !inv_source.is_empty() && inv_source != "local-calendar" {
        if let Some(idx) = sources.iter().position(|(uid, _)| uid == inv_source) {
            return Some(idx);
        }
    }

    // 4. Default to first calendar (Personal Calendar)
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::calendar_store_service::{CalendarEventRecord, CalendarSnapshot};

    #[test]
    fn parses_various_date_formats() {
        assert_eq!(
            parse_start_date("2026-08-10"),
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        );
        assert_eq!(
            parse_start_date("20260810T120000Z"),
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        );
        assert_eq!(
            parse_start_date("10.08.2026 12:00"),
            NaiveDate::from_ymd_opt(2026, 8, 10).unwrap()
        );
    }

    #[test]
    fn test_itip_banner_cancelled_meeting_hides_response_pending_badge() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let banner = ItipBanner::new();
        let invitation = ItipInvitation {
            method: ItipMethod::Cancel,
            event: crate::services::calendar_store_service::CalendarEventRecord {
                uid: "cancel-1".to_string(),
                source_uid: "local-calendar".to_string(),
                source_display_name: "Calendar".to_string(),
                component: "".to_string(),
                summary: "Abgesagt: Besprechung".to_string(),
                start: "2026-10-16 09:00".to_string(),
                end: "2026-10-16 09:30".to_string(),
                location: "Konferenzraum".to_string(),
                description: "".to_string(),
                all_day: false,
            },
            organizer: "boss@example.com".to_string(),
            attendees: vec!["user@example.com".to_string()],
            organizer_person: None,
            attendee_persons: vec![],
            sequence: Some(1),
            recurrence: None,
            raw_component: "".to_string(),
        };

        banner.set_invitation(Some(invitation), &["user@example.com"]);
        assert!(
            !banner.lbl_status_badge.is_visible(),
            "Cancelled meeting must not display status badge ('Response Pending')"
        );
        assert!(
            banner.action_box_cancel.is_visible(),
            "Cancelled meeting must display action_box_cancel"
        );
        assert!(
            !banner.action_box_request.is_visible(),
            "Cancelled meeting must not display action_box_request"
        );
        assert!(
            !banner.exp_description.is_visible(),
            "Empty description should keep expander hidden"
        );
    }

    #[test]
    fn test_itip_banner_request_shows_response_pending_badge_and_expander() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let banner = ItipBanner::new();
        let invitation = ItipInvitation {
            method: ItipMethod::Request,
            event: crate::services::calendar_store_service::CalendarEventRecord {
                uid: "req-1".to_string(),
                source_uid: "local-calendar".to_string(),
                source_display_name: "Calendar".to_string(),
                component: "".to_string(),
                summary: "Projektbesprechung".to_string(),
                start: "2026-10-16 09:00".to_string(),
                end: "2026-10-16 09:30".to_string(),
                location: "https://meet.example.com/xyz".to_string(),
                description: "Agenda:\n1. Roadmap\n2. Q&A".to_string(),
                all_day: false,
            },
            organizer: "boss@example.com".to_string(),
            attendees: vec!["user@example.com".to_string()],
            organizer_person: None,
            attendee_persons: vec![],
            sequence: Some(0),
            recurrence: None,
            raw_component: "".to_string(),
        };

        banner.set_invitation(Some(invitation), &["user@example.com"]);
        assert!(
            banner.lbl_status_badge.is_visible(),
            "Request invitation must display status badge"
        );
        assert_eq!(
            banner.lbl_status_badge.text(),
            crate::i18n::gettext("Response Pending")
        );
        assert!(
            banner.action_box_request.is_visible(),
            "Request invitation must display action_box_request"
        );
        assert!(
            !banner.exp_description.is_visible(),
            "Redundant description expander should be hidden"
        );
        assert!(
            banner.timeline_side_box.is_visible(),
            "Timeline strip should be visible for request invitation"
        );
        assert_eq!(
            banner.lbl_description.text(),
            "Agenda:\n1. Roadmap\n2. Q&A"
        );
        assert!(
            banner.lbl_title.wraps(),
            "Event title label must have word-wrapping enabled"
        );
    }

    #[test]
    fn test_calendar_conflict_detection() {
        use crate::services::calendar_store_service::{CalendarEventRecord, CalendarSnapshot};

        let existing_ev = CalendarEventRecord {
            uid: "ev-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Team Sync".to_string(),
            start: "2026-10-16 09:00".to_string(),
            end: "2026-10-16 10:00".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };

        let snapshot = CalendarSnapshot {
            events: vec![existing_ev],
            tasks: Vec::new(),
            memos: Vec::new(),
        };

        // 1. Overlapping event: 09:30 - 10:30 on same day -> Conflict!
        let overlap_req = CalendarEventRecord {
            uid: "req-overlap".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Kundenmeeting".to_string(),
            start: "2026-10-16 09:30".to_string(),
            end: "2026-10-16 10:30".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let (has_conflict, conflicts) = check_calendar_conflicts_with_snapshot(&overlap_req, &snapshot);
        assert!(has_conflict, "Overlapping time range must report conflict");
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("Team Sync"));

        // 2. Non-overlapping event: 11:00 - 12:00 -> No conflict!
        let free_req = CalendarEventRecord {
            uid: "req-free".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Freie Zeit".to_string(),
            start: "2026-10-16 11:00".to_string(),
            end: "2026-10-16 12:00".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let (has_conflict, conflicts) = check_calendar_conflicts_with_snapshot(&free_req, &snapshot);
        assert!(!has_conflict, "Disjoint time slot must not report conflict");
        assert!(conflicts.is_empty());

        // 3. Different day -> No conflict!
        let diff_day_req = CalendarEventRecord {
            uid: "req-diff-day".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Anderer Tag".to_string(),
            start: "2026-10-17 09:30".to_string(),
            end: "2026-10-17 10:30".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let (has_conflict, _conflicts) = check_calendar_conflicts_with_snapshot(&diff_day_req, &snapshot);
        assert!(!has_conflict, "Event on different day must not report conflict");

        // 4. Same UID (e.g. updating self) -> Ignored, no conflict!
        let self_req = CalendarEventRecord {
            uid: "ev-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Team Sync Update".to_string(),
            start: "2026-10-16 09:00".to_string(),
            end: "2026-10-16 10:00".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let (has_conflict, _conflicts) = check_calendar_conflicts_with_snapshot(&self_req, &snapshot);
        assert!(!has_conflict, "Same event UID must not conflict with itself");
    }

    #[test]
    fn test_day_timeline_side_strip_visibility_and_conflict_badge() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let banner = ItipBanner::new();
        banner.revealer.set_visible(true);
        let target_ev = CalendarEventRecord {
            uid: "target-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Gin, Jazz & Glitzer – Mitarbeiterfest".to_string(),
            start: "2026-10-16 14:00".to_string(),
            end: "2026-10-16 18:00".to_string(),
            location: "Festsaal".to_string(),
            description: "Willkommen!".to_string(),
            all_day: false,
        };

        // 1. Snapshot with conflict (15:00 - 16:00)
        let conflict_ev = CalendarEventRecord {
            uid: "existing-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Wöchentliches Meeting".to_string(),
            start: "2026-10-16 15:00".to_string(),
            end: "2026-10-16 16:00".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let snapshot_conflict = CalendarSnapshot {
            events: vec![conflict_ev],
            tasks: Vec::new(),
            memos: Vec::new(),
        };

        banner.update_conflicts_and_timeline(&target_ev, &snapshot_conflict);
        assert!(banner.timeline_side_box.is_visible(), "Timeline side strip should be directly visible");
        assert!(banner.lbl_timeline_badge.is_visible(), "Timeline conflict badge should be visible on conflict");
        assert!(banner.lbl_conflict.text().contains("Wöchentliches Meeting"));

        // 2. Snapshot without conflict
        let snapshot_free = CalendarSnapshot {
            events: Vec::new(),
            tasks: Vec::new(),
            memos: Vec::new(),
        };
        banner.update_conflicts_and_timeline(&target_ev, &snapshot_free);
        assert!(banner.timeline_side_box.is_visible(), "Timeline side strip should remain directly visible");
        assert!(!banner.lbl_timeline_badge.is_visible(), "Badge should be hidden when free of conflicts");
        assert_eq!(banner.lbl_conflict.text(), gettext("No scheduling conflicts"));
    }

    #[test]
    fn test_safe_date_and_time_parsing_resilience() {
        // Multi-byte chars and emojis with embedded date are parsed safely
        let d1 = parse_start_date("🎉 2026-12-24 🎁");
        assert_eq!(d1, NaiveDate::from_ymd_opt(2026, 12, 24).unwrap());

        // Completely invalid string falls back to today without panic
        let d_invalid = parse_start_date("kein-datum-vorhanden");
        assert_eq!(d_invalid, chrono::Local::now().date_naive());

        let d2 = parse_start_date("2026-10-16T14:30:00Z");
        assert_eq!(d2, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());

        let d3 = parse_start_date("16.10.2026");
        assert_eq!(d3, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());

        let (date, min) = extract_date_and_minutes("2026-10-16 14:30");
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        assert_eq!(min, Some(14 * 60 + 30));

        let (date2, min2) = extract_date_and_minutes("20261016T090000Z");
        assert_eq!(date2, NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        assert_eq!(min2, Some(9 * 60));

        let (_, min_invalid) = extract_date_and_minutes("keine-uhrzeit");
        assert_eq!(min_invalid, None);

        let (_, min_weird) = extract_date_and_minutes("Gin, Jazz & Glitzer – Welcome to the 20s");
        assert_eq!(min_weird, None);
    }

    #[test]
    fn test_evolution_itip_layout_elements() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let banner = ItipBanner::new();
        banner.revealer.set_visible(true);

        // 1. Initial state
        assert!(!banner.sender_message_box.is_visible());
        assert!(banner.chk_send_reply.is_active());
        assert_eq!(banner.comment_text(), None);

        // 2. Setting sender message body
        banner.set_email_body(Some("Hallo Martin, können wir das kurz besprechen?"));
        assert!(banner.sender_message_box.is_visible());
        assert_eq!(
            banner.lbl_sender_message.text(),
            "Hallo Martin, können wir das kurz besprechen?"
        );

        // 3. Comment view text extraction
        banner.comment_view.buffer().set_text("Ich komme 5 Minuten später");
        assert_eq!(
            banner.comment_text(),
            Some("Ich komme 5 Minuten später".to_string())
        );

        // 4. Clearing email body
        banner.set_email_body(None);
        assert!(!banner.sender_message_box.is_visible());

        // 5. Test iCalendar manipulation helpers
        let sample_cal = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Test\r\nORGANIZER:mailto:boss@corp.com\r\nATTENDEE:mailto:worker@corp.com\r\nCLASS:PUBLIC\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let stripped = strip_organizer_and_attendees(sample_cal);
        assert!(!stripped.contains("ORGANIZER"));
        assert!(!stripped.contains("ATTENDEE"));
        assert!(stripped.contains("SUMMARY:Test"));

        let transparent = set_component_transparency(sample_cal, true);
        assert!(transparent.contains("TRANSP:TRANSPARENT"));

        // 6. Sender message card & Day View timeline parity
        assert!(banner.sender_message_box.has_css_class("card"));
        assert!(banner.sender_message_box.has_css_class("mail-preview-card"));
        assert!(banner.timeline_side_box.has_css_class("calendar-time-grid"));

        let target_ev = CalendarEventRecord {
            uid: "target-lunch".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Lunch".to_string(),
            start: "2026-08-12 13:20".to_string(),
            end: "2026-08-12 13:50".to_string(),
            location: "Kochschule".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let existing_ev = CalendarEventRecord {
            uid: "existing-standup".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Calendar".to_string(),
            component: "".to_string(),
            summary: "Daily Standup".to_string(),
            start: "2026-08-12 17:15".to_string(),
            end: "2026-08-12 18:15".to_string(),
            location: "".to_string(),
            description: "".to_string(),
            all_day: false,
        };
        let snapshot = CalendarSnapshot {
            events: vec![existing_ev],
            tasks: Vec::new(),
            memos: Vec::new(),
        };
        banner.update_conflicts_and_timeline(&target_ev, &snapshot);

        let mut found_existing_title = false;
        let mut child = banner.timeline_fixed.first_child();
        while let Some(w) = child {
            if let Some(lbl) = w.downcast_ref::<gtk::Label>() {
                if lbl.label().contains("Daily Standup") {
                    found_existing_title = true;
                }
            }
            child = w.next_sibling();
        }
        assert!(
            found_existing_title,
            "Existing timed event on day timeline must display its summary title ('Daily Standup')"
        );
    }

    #[test]
    fn test_smart_calendar_heuristic_and_target_chip() {
        let sources = vec![
            ("local-calendar".to_string(), "Personal Calendar".to_string()),
            ("eds-work-1".to_string(), "Work (martin@acme.corp)".to_string()),
            ("eds-google-1".to_string(), "Google Calendar".to_string()),
        ];

        let base_inv = ItipInvitation {
            method: ItipMethod::Request,
            event: CalendarEventRecord {
                uid: "inv-uid-42".to_string(),
                source_uid: "local-calendar".to_string(),
                source_display_name: "Calendar".to_string(),
                component: "".to_string(),
                summary: "Architecture Review".to_string(),
                start: "2026-10-20 10:00".to_string(),
                end: "2026-10-20 11:00".to_string(),
                location: "".to_string(),
                description: "".to_string(),
                all_day: false,
            },
            organizer: "cto@acme.corp".to_string(),
            attendees: vec!["mailto:martin@acme.corp".to_string()],
            organizer_person: None,
            attendee_persons: vec![],
            sequence: Some(0),
            recurrence: None,
            raw_component: "".to_string(),
        };

        let empty_snap = CalendarSnapshot {
            events: vec![],
            tasks: vec![],
            memos: vec![],
        };

        // 1. Matches work calendar by invited email / domain
        let idx_work = find_best_calendar_source_index(
            &sources,
            &base_inv,
            &["martin@acme.corp", "m.jensen@gmail.com"],
            &empty_snap,
        );
        assert_eq!(idx_work, Some(1));

        // 2. Matches Google calendar by Gmail address
        let mut gmail_inv = base_inv.clone();
        gmail_inv.attendees = vec!["mailto:m.jensen@gmail.com".to_string()];
        let idx_google = find_best_calendar_source_index(
            &sources,
            &gmail_inv,
            &["m.jensen@gmail.com"],
            &empty_snap,
        );
        assert_eq!(idx_google, Some(2));

        // 3. Existing event UID in snapshot takes highest priority
        let snap_with_existing = CalendarSnapshot {
            events: vec![CalendarEventRecord {
                uid: "inv-uid-42".to_string(),
                source_uid: "eds-google-1".to_string(),
                source_display_name: "Google Calendar".to_string(),
                component: "".to_string(),
                summary: "Architecture Review".to_string(),
                start: "2026-10-20 10:00".to_string(),
                end: "2026-10-20 11:00".to_string(),
                location: "".to_string(),
                description: "".to_string(),
                all_day: false,
            }],
            tasks: vec![],
            memos: vec![],
        };
        let idx_existing = find_best_calendar_source_index(
            &sources,
            &base_inv,
            &["martin@acme.corp"],
            &snap_with_existing,
        );
        assert_eq!(idx_existing, Some(2));
    }

    #[test]
    fn test_dynamic_timeline_width_and_smart_location_queries() {
        // Minimum width is strictly 280px
        assert_eq!(compute_dynamic_timeline_width(620, true, 1), 280);
        assert_eq!(compute_dynamic_timeline_width(640, false, 1), 280);

        // On wide preview panes with compact metadata and parallel columns, calendar expands smoothly
        let w_compact_wide = compute_dynamic_timeline_width(920, true, 3);
        let w_full_msg_wide = compute_dynamic_timeline_width(920, false, 1);
        assert!(w_compact_wide >= 380);
        assert!(w_compact_wide > w_full_msg_wide);

        // Smart location queries strip room prefixes and fall back to city name
        let queries = smart_location_queries("Konferenzraum Wiesbaden P&I");
        assert_eq!(queries[0], "Konferenzraum Wiesbaden P&I");
        assert!(queries.contains(&"Wiesbaden P&I".to_string()));
        assert!(queries.contains(&"Wiesbaden".to_string()));

        // Extract GEO coordinates from VEVENT
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nGEO:50.0826;8.2400\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let geo = extract_ical_geo(ics).expect("should parse GEO");
        assert!((geo.0 - 50.0826).abs() < 1e-4);
        assert!((geo.1 - 8.2400).abs() < 1e-4);
    }

    #[test]
    fn test_calendar_picker_account_grouping_and_high_contrast_dots() {
        let raw = vec![
            (
                "cal-arbeit".to_string(),
                "Arbeit".to_string(),
                Some("mjensen@acme-corp.example".to_string()),
            ),
            (
                "cal-familie".to_string(),
                "Familie".to_string(),
                Some("mjensen@acme-corp.example".to_string()),
            ),
            (
                "cal-main".to_string(),
                "Kalender (mjensen@acme-corp.example)".to_string(),
                Some("mjensen@acme-corp.example".to_string()),
            ),
            (
                "cal-business".to_string(),
                "Business".to_string(),
                Some("mjensen@acme-corp.example".to_string()),
            ),
        ];

        let items = group_and_sort_calendar_sources(raw);
        // Primary calendar ("Kalender") of the account must be placed first in its account group
        assert_eq!(items[0].uid, "cal-main");
        assert_eq!(items[0].group_name, "mjensen@acme-corp.example");
        assert_eq!(items[0].clean_name, "Kalender");
        assert_eq!(items[0].picker_label, "Kalender (mjensen@acme-corp.example)");

        // Followed alphabetically by Arbeit, Business, Familie under the same account
        assert_eq!(items[1].clean_name, "Arbeit");
        assert_eq!(items[2].clean_name, "Business");
        assert_eq!(items[3].clean_name, "Familie");

        // Local fallback placed last under "On This Computer"
        assert_eq!(items[4].uid, "local-calendar");

        // High-contrast dot colors for very pale colors in light mode
        let (fill_pale, border_pale) = calendar_dot_colors_for_hex("#e0e0e0", false);
        assert_ne!(fill_pale, "#e0e0e0");
        assert_ne!(border_pale, "#e0e0e0");
        let (_, border_yellow) = calendar_dot_colors_for_hex("#ffff66", false);
        assert_ne!(border_yellow, "#ffff66");
    }
}



