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
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::itip_service::{
    find_user_status, format_recurrence_summary, update_attendee_partstat,
    ItipInvitation, ItipMethod, ItipReplyStatus,
};

#[derive(Clone)]
pub struct ItipBanner {
    revealer: gtk::Revealer,
    banner_box: gtk::Box,
    details_revealer: gtk::Revealer,
    cal_select_box: gtk::Box,
    footer_box: gtk::Box,
    is_compact: Rc<Cell<bool>>,
    icon: gtk::Image,
    lbl_header_prompt: gtk::Label,
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
    timeline_fixed: gtk::Fixed,
    timeline_scrolled: gtk::ScrolledWindow,
    timeline_target_scroll_y: Rc<Cell<f64>>,
    lbl_location: gtk::Label,
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
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .reveal_child(false)
            .visible(false)
            .hexpand(true)
            .build();

        let banner_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(vec!["itip-banner"])
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(12)
            .build();

        // ── Top Header Prompt ─────────────────────────────────────────────────
        let header_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .hexpand(true)
            .valign(gtk::Align::Center)
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
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .build();
        header_row.append(&lbl_status_badge);
        banner_box.append(&header_row);

        let sep_top = gtk::Separator::new(gtk::Orientation::Horizontal);
        banner_box.append(&sep_top);

        // ── Main 2-Column Content ─────────────────────────────────────────────
        let details_vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .valign(gtk::Align::Fill)
            .build();

        // 1. Event Title
        let lbl_title = gtk::Label::builder()
            .label(&gettext("Event"))
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .width_chars(1)
            .build();
        details_vbox.append(&lbl_title);

        // 2. Metadata Rows
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
            .css_classes(vec!["body"])
            .build();
        time_row.append(&time_icon);
        time_row.append(&lbl_time);
        details_vbox.append(&time_row);

        let conflict_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let conflict_icon = gtk::Image::builder()
            .icon_name("emblem-ok-symbolic")
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
        // NOTE: conflict_row is kept for test compatibility but not added to details_vbox.
        // The right timeline strip header is the single source of truth for scheduling conflicts.

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
        details_vbox.append(&recurrence_row);

        let location_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .visible(false)
            .build();
        let loc_icon = gtk::Image::builder()
            .icon_name("find-location-symbolic")
            .valign(gtk::Align::Start)
            .margin_top(2)
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_location = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(vec!["body"])
            .build();
        location_row.append(&loc_icon);
        location_row.append(&lbl_location);
        details_vbox.append(&location_row);

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
        details_vbox.append(&organizer_row);

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
            .width_chars(1)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        attendees_row.append(&att_icon);
        attendees_row.append(&lbl_attendees);
        details_vbox.append(&attendees_row);

        // 3. Sender Message / Description (DIRECTLY VISIBLE, NOT HIDDEN BEHIND EXPANDER!)
        let sender_message_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["itip-sender-message-box"])
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
            .selectable(true)
            .css_classes(vec!["body"])
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

        // 6. Comment Box (Progressive Disclosure - Revealed on demand)
        let comment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .margin_top(4)
            .margin_bottom(6)
            .build();
        let lbl_comment = gtk::Label::builder()
            .label(&format!("{}:", gettext("Comment")))
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
        details_vbox.append(&comment_revealer);

        // Separator before actions
        let sep_footer = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep_footer.set_margin_top(6);
        sep_footer.set_margin_bottom(2);
        details_vbox.append(&sep_footer);

        // 7. Action Buttons Row (GNOME HIG)
        let footer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .margin_top(2)
            .margin_bottom(2)
            .build();

        let open_calendar_label = gettext("Open in Calendar");
        let btn_open_content = adw::ButtonContent::builder()
            .icon_name("x-office-calendar-symbolic")
            .label(&open_calendar_label)
            .build();
        let btn_open_calendar = gtk::Button::builder()
            .child(&btn_open_content)
            .css_classes(vec!["flat"])
            .tooltip_text(&gettext("Switch to calendar and show date"))
            .valign(gtk::Align::Center)
            .build();
        footer_box.append(&btn_open_calendar);

        let action_spacer = gtk::Box::builder().hexpand(true).build();
        footer_box.append(&action_spacer);

        // Request actions
        let action_box_request = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();

        let comment_btn_label = gettext("Comment");
        let btn_comment_content = adw::ButtonContent::builder()
            .icon_name("chat-bubbles-symbolic")
            .label(&comment_btn_label)
            .build();
        let btn_toggle_comment = gtk::ToggleButton::builder()
            .child(&btn_comment_content)
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
        action_box_request.append(&btn_toggle_comment);

        let make_shrinkable_btn = |text: &str, classes: Vec<&str>, tooltip: &str| -> gtk::Button {
            let lbl = gtk::Label::builder()
                .label(text)
                .ellipsize(pango::EllipsizeMode::End)
                .width_chars(1)
                .build();
            gtk::Button::builder()
                .child(&lbl)
                .css_classes(classes)
                .tooltip_text(tooltip)
                .build()
        };

        let btn_decline = make_shrinkable_btn(
            &gettext("Decline"),
            vec!["destructive-action"],
            &gettext("Decline invitation and send response"),
        );
        let btn_tentative = make_shrinkable_btn(
            &gettext("Tentative"),
            vec![],
            &gettext("Tentatively accept and add to calendar"),
        );
        let btn_accept = make_shrinkable_btn(
            &gettext("Accept"),
            vec!["suggested-action"],
            &gettext("Accept event, add to calendar and send response"),
        );
        action_box_request.append(&btn_decline);
        action_box_request.append(&btn_tentative);
        action_box_request.append(&btn_accept);

        // More options popover (Target Calendar, Reply toggle, Free/Busy, Import)
        let more_popover = gtk::Popover::new();
        let pop_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .width_request(260)
            .build();

        let cal_lbl = gtk::Label::builder()
            .label(&gettext("Target Calendar"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading", "dim-label"])
            .build();
        pop_box.append(&cal_lbl);
        pop_box.append(&cal_select_box);

        let sep_pop1 = gtk::Separator::new(gtk::Orientation::Horizontal);
        pop_box.append(&sep_pop1);

        let opts_lbl = gtk::Label::builder()
            .label(&gettext("Options"))
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading", "dim-label"])
            .build();
        pop_box.append(&opts_lbl);
        pop_box.append(&chk_send_reply);
        pop_box.append(&chk_show_free);
        pop_box.append(&chk_preserve_reminder);

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
        footer_box.append(&action_box_request);

        // Cancel actions
        let action_box_cancel = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let btn_remove = gtk::Button::builder()
            .label(&gettext("Remove from Calendar"))
            .css_classes(vec!["destructive-action"])
            .tooltip_text(&gettext("Remove cancelled event from your calendar"))
            .build();
        action_box_cancel.append(&btn_remove);
        footer_box.append(&action_box_cancel);

        // Reply actions
        let action_box_reply = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let btn_update = gtk::Button::builder()
            .label(&gettext("Update Calendar"))
            .css_classes(vec!["suggested-action"])
            .tooltip_text(&gettext("Update attendee status in existing calendar event"))
            .build();
        action_box_reply.append(&btn_update);
        footer_box.append(&action_box_reply);

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
        footer_box.append(&action_box_publish);

        details_vbox.append(&footer_box);

        // ── Right Column: Day Schedule / Timeline Strip (Evolution Parity) ──
        let timeline_side_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(220)
            .valign(gtk::Align::Fill)
            .css_classes(vec!["itip-day-timeline-container"])
            .visible(false)
            .build();

        let timeline_top_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .margin_start(4)
            .margin_end(4)
            .margin_top(2)
            .build();

        let lbl_timeline_date = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes(vec!["heading", "dim-label"])
            .build();
        timeline_top_bar.append(&lbl_timeline_date);

        let lbl_timeline_badge = gtk::Label::builder()
            .label("")
            .css_classes(vec!["appointment-timeline-conflict-badge"])
            .visible(false)
            .build();
        timeline_top_bar.append(&lbl_timeline_badge);
        timeline_side_box.append(&timeline_top_bar);

        let timeline_all_day_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(4)
            .margin_end(4)
            .margin_top(2)
            .margin_bottom(4)
            .visible(false)
            .build();
        timeline_side_box.append(&timeline_all_day_box);

        let timeline_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .height_request(280)
            .vexpand(true)
            .css_classes(vec!["itip-timeline-scrolled"])
            .build();

        let timeline_overlay = gtk::Overlay::new();
        let hours_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        for h in 0..24 {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .height_request(HOUR_HEIGHT as i32)
                .css_classes(vec!["appointment-timeline-hour-row"])
                .build();
            let lbl = gtk::Label::builder()
                .label(&format!("{h:02}:00"))
                .width_request(40)
                .xalign(1.0)
                .css_classes(vec!["appointment-timeline-hour-label"])
                .build();
            let sep_hour = gtk::Separator::builder()
                .orientation(gtk::Orientation::Horizontal)
                .hexpand(true)
                .valign(gtk::Align::Start)
                .build();
            row.append(&lbl);
            row.append(&sep_hour);
            hours_box.append(&row);
        }
        timeline_overlay.set_child(Some(&hours_box));

        let timeline_fixed = gtk::Fixed::builder()
            .height_request((24.0 * HOUR_HEIGHT) as i32)
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
            .spacing(20)
            .hexpand(true)
            .valign(gtk::Align::Fill)
            .build();
        details_columns.append(&details_vbox);
        details_columns.append(&timeline_side_box);

        let details_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .propagate_natural_height(true)
            .min_content_width(0)
            .hexpand(true)
            .vexpand(true)
            .child(&details_columns)
            .build();

        let details_revealer = gtk::Revealer::builder()
            .reveal_child(true)
            .hexpand(true)
            .vexpand(true)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(200)
            .child(&details_scrolled)
            .build();
        banner_box.append(&details_revealer);
        revealer.set_child(Some(&banner_box));

        // Responsive adaptation when the preview pane is narrowed
        {
            let cols = details_columns.clone();
            let tl_box = timeline_side_box.clone();
            let f_box = footer_box.clone();
            let spacer = action_spacer.clone();
            let open_btn = btn_open_calendar.clone();
            let req_box = action_box_request.clone();
            let open_content = btn_open_content.clone();
            let comment_content = btn_comment_content.clone();
            let open_lbl_full = open_calendar_label;
            let comment_lbl_full = comment_btn_label;
            let last_width = Cell::new(0);
            let last_tl_vis = Cell::new(false);
            banner_box.add_tick_callback(move |widget, _| {
                let w = widget.width();
                let tl_vis = tl_box.is_visible();
                if w > 0 && (w != last_width.get() || tl_vis != last_tl_vis.get()) {
                    last_width.set(w);
                    last_tl_vis.set(tl_vis);
                    let stack_columns = w < 620;
                    if stack_columns {
                        if cols.orientation() != gtk::Orientation::Vertical {
                            cols.set_orientation(gtk::Orientation::Vertical);
                            cols.set_spacing(12);
                        }
                        tl_box.set_width_request(-1);
                        tl_box.set_hexpand(true);
                        tl_box.add_css_class("is-stacked");
                    } else {
                        if cols.orientation() != gtk::Orientation::Horizontal {
                            cols.set_orientation(gtk::Orientation::Horizontal);
                            cols.set_spacing(20);
                        }
                        tl_box.set_width_request(220);
                        tl_box.set_hexpand(false);
                        tl_box.remove_css_class("is-stacked");
                    }

                    let effective_col_w = if !stack_columns && tl_vis {
                        (w - 240).max(0)
                    } else {
                        w
                    };

                    let compact_buttons = effective_col_w < 540;
                    let stack_footer = effective_col_w < 400;

                    open_content.set_label(if compact_buttons && !stack_footer {
                        ""
                    } else {
                        &open_lbl_full
                    });
                    comment_content.set_label(if compact_buttons {
                        ""
                    } else {
                        &comment_lbl_full
                    });

                    let target_footer_orient = if stack_footer {
                        gtk::Orientation::Vertical
                    } else {
                        gtk::Orientation::Horizontal
                    };
                    if f_box.orientation() != target_footer_orient {
                        f_box.set_orientation(target_footer_orient);
                    }
                    spacer.set_visible(!stack_footer);
                    open_btn.set_halign(if stack_footer {
                        gtk::Align::Start
                    } else {
                        gtk::Align::Fill
                    });
                    req_box.set_halign(if stack_footer {
                        gtk::Align::Start
                    } else {
                        gtk::Align::End
                    });
                }
                glib::ControlFlow::Continue
            });
        }

        let banner = Self {
            revealer,
            banner_box,
            details_revealer,
            cal_select_box,
            footer_box,
            is_compact: Rc::new(Cell::new(false)),
            icon,
            lbl_header_prompt,
            lbl_title,
            lbl_method,
            lbl_status_badge,
            lbl_time,
            conflict_row,
            conflict_icon,
            lbl_conflict,
            timeline_side_box,
            lbl_timeline_date,
            lbl_timeline_badge,
            timeline_all_day_box,
            timeline_fixed,
            timeline_scrolled,
            timeline_target_scroll_y,
            lbl_location,
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
        };

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
        self.footer_box.set_margin_top(if compact { 0 } else { 4 });
        if compact {
            self.banner_box.add_css_class("is-compact");
            self.icon.set_pixel_size(22);
        } else {
            self.banner_box.remove_css_class("is-compact");
            self.icon.set_pixel_size(24);
        }
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
            self.calendar_status_icon.set_icon_name(Some("emblem-ok-symbolic"));
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

    fn setup_calendar_sources(&self) {
        let sources = vec![("local-calendar".to_string(), gettext("Personal Calendar"))];
        let names: Vec<&str> = sources.iter().map(|(_, name)| name.as_str()).collect();
        let model = gtk::StringList::new(&names);
        self.calendar_dropdown.set_model(Some(&model));
        self.calendar_dropdown.set_selected(0);
        *self.calendar_sources.borrow_mut() = sources;

        let dropdown = self.calendar_dropdown.clone();
        let sources_store = Rc::clone(&self.calendar_sources);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            if let Ok(discovery) = ECalService::new().discover_source_capabilities(ECalComponentType::Event) {
                let _ = tx.send_blocking(discovery);
            }
        });
        glib::spawn_future_local(async move {
            if let Ok(discovery) = rx.recv().await {
                let mut sources = vec![("local-calendar".to_string(), gettext("Personal Calendar"))];
                for source in discovery.sources {
                    if source.writable && source.source_uid != "local-calendar" {
                        sources.push((source.source_uid, source.display_name));
                    }
                }
                let names: Vec<&str> = sources.iter().map(|(_, name)| name.as_str()).collect();
                let model = gtk::StringList::new(&names);
                dropdown.set_model(Some(&model));
                *sources_store.borrow_mut() = sources;
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
                if !org_name.is_empty() {
                    self.lbl_header_prompt.set_text(&gettext("{} has cancelled the following meeting:").replace("{}", &org_name));
                } else {
                    self.lbl_header_prompt.set_text(&gettext("Meeting has been cancelled"));
                }
                self.banner_box.add_css_class("itip-cancelled");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(true);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
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
                self.chk_send_reply.set_visible(false);
                self.comment_view.set_visible(false);
                self.chk_show_free.set_visible(false);
                self.chk_preserve_reminder.set_visible(false);
                self.btn_import_as_event.set_visible(false);
                self.lbl_import_hint.set_visible(false);
            }
        }

        self.comment_view.buffer().set_text("");

        // Time / Date range
        let time_str = if event.all_day {
            format!("{}: {}", gettext("All day"), event.start)
        } else if !event.end.trim().is_empty() {
            format!("{} – {}", event.start, event.end)
        } else {
            event.start.clone()
        };
        self.lbl_time.set_text(&time_str);

        // Calendar conflict check and Day Timeline
        let initial_snapshot = load_local_calendar_snapshot_safe();
        self.update_calendar_presence_status(event, &initial_snapshot);

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
        let loc = event.location.trim();
        if !loc.is_empty() {
            if loc.starts_with("http://") || loc.starts_with("https://") {
                self.lbl_location.set_markup(&format!(
                    "<a href=\"{}\">{}</a>",
                    glib::markup_escape_text(loc),
                    glib::markup_escape_text(loc)
                ));
            } else {
                self.lbl_location.set_text(loc);
            }
            self.location_row.set_visible(true);
        } else {
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

        // Organizer
        if let Some(person) = &invitation.organizer_person {
            let display_name = person.display_label();
            if !display_name.is_empty() {
                self.lbl_organizer.set_text(&format!("{}: {display_name}", gettext("Organizer")));
                let tooltip = person.full_display();
                self.lbl_organizer.set_tooltip_text(Some(&tooltip));
                self.organizer_row.set_tooltip_text(Some(&tooltip));
                self.organizer_row.set_visible(true);
            } else {
                self.organizer_row.set_visible(false);
            }
        } else if !invitation.organizer.trim().is_empty() {
            let org = crate::services::itip_service::strip_mailto_prefix(invitation.organizer.trim());
            self.lbl_organizer.set_text(&format!("{}: {org}", gettext("Organizer")));
            self.lbl_organizer.set_tooltip_text(Some(org));
            self.organizer_row.set_tooltip_text(Some(org));
            self.organizer_row.set_visible(true);
        } else {
            self.organizer_row.set_visible(false);
        }

        // Attendees
        let attendees = &invitation.attendee_persons;
        if !attendees.is_empty() {
            let count = attendees.len();
            let label_text = if count == 1 {
                format!("{}: {}", gettext("Attendee"), attendees[0].display_label())
            } else if count <= 3 {
                let names = attendees
                    .iter()
                    .map(|p| p.display_label())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}: {names}", gettext("Attendees"))
            } else {
                let names_preview = attendees
                    .iter()
                    .take(2)
                    .map(|p| p.display_label())
                    .collect::<Vec<_>>()
                    .join(", ");
                let base = gettext("{} attendees").replace("{}", &count.to_string());
                format!("{base} ({names_preview}, …)")
            };

            let tooltip_lines = attendees
                .iter()
                .map(|p| p.full_display())
                .collect::<Vec<_>>()
                .join("\n");

            self.lbl_attendees.set_text(&label_text);
            self.lbl_attendees.set_tooltip_text(Some(&tooltip_lines));
            self.attendees_row.set_tooltip_text(Some(&tooltip_lines));
            self.attendees_row.set_visible(true);
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
            self.attendees_row.set_visible(true);
        } else {
            self.attendees_row.set_visible(false);
        }

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
            self.conflict_icon.set_icon_name(Some("emblem-ok-symbolic"));
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
                format!("⚠️ 1 {}", gettext("Conflict"))
            } else {
                format!("⚠️ {conflict_count} {}", gettext("Conflicts"))
            };
            self.lbl_timeline_badge.set_label(&badge_text);
            self.lbl_timeline_badge.set_tooltip_text(Some(&conflicts.join("\n")));
            self.lbl_timeline_badge.set_visible(true);
        } else {
            self.lbl_timeline_badge.set_tooltip_text(None);
            self.lbl_timeline_badge.set_visible(false);
        }
        self.timeline_side_box.set_visible(true);

        let scroll_hour = target_start_min.map(|m| (m / 60) as f64).unwrap_or(8.0);
        let target_scroll_y = ((scroll_hour - 1.0).max(0.0)) * HOUR_HEIGHT;
        self.timeline_target_scroll_y.set(target_scroll_y);
        let vadj = self.timeline_scrolled.vadjustment();
        glib::idle_add_local_once(move || {
            vadj.set_value(target_scroll_y);
        });

        let is_dark = adw::StyleManager::default().is_dark();

        // 1. All-day items
        let mut has_all_day = false;
        if target_event.all_day {
            has_all_day = true;
            let title = if target_event.summary.trim().is_empty() {
                gettext("(Untitled)")
            } else {
                target_event.summary.clone()
            };
            let chip = gtk::Label::builder()
                .label(&format!("⭐ {}: {title}", gettext("Invitation")))
                .halign(gtk::Align::Fill)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(vec!["appointment-timeline-event", "appointment-timeline-preview", "itip-timeline-target-all-day"])
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
                    ev.summary.clone()
                };
                let chip = gtk::Label::builder()
                    .label(&format!("{}: {title}", gettext("All Day")))
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
                let (ev_start, ev_end, ev_title) =
                    crate::views::calendar_page::event_occurrence_times_for_date(ev, target_date, 30);
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
                    target_event.summary.clone()
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

        const TIMELINE_EVENTS_X: f64 = 44.0;
        let total_width: f64 = 200.0;

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
                ((total_width - (total_cols as f64 - 1.0) * col_gap) / total_cols as f64).max(40.0)
            } else {
                total_width
            };

            for (item, col_idx) in placed {
                let x = TIMELINE_EVENTS_X + col_idx as f64 * (col_width + col_gap);
                let y = (item.start_min as f64 / 60.0) * HOUR_HEIGHT;
                let duration = item.end_min.saturating_sub(item.start_min);
                let h = ((duration.max(20) as f64) / 60.0) * HOUR_HEIGHT - 2.0;

                let is_conflict = item
                    .uid
                    .as_ref()
                    .map(|u| conflicting_uids.contains(u))
                    .unwrap_or(false)
                    || (item.is_target && has_conflict);

                let time_prefix = format!("{:02}:{:02}", item.start_min / 60, item.start_min % 60);
                let time_end = format!("{:02}:{:02}", item.end_min / 60, item.end_min % 60);
                let label_text = format!("{} {}", time_prefix, item.title);

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

                let mut css_classes = vec!["appointment-timeline-event"];
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
                    .label(&label_text)
                    .ellipsize(pango::EllipsizeMode::End)
                    .xalign(0.0)
                    .halign(gtk::Align::Fill)
                    .valign(gtk::Align::Fill)
                    .width_request(col_width as i32)
                    .height_request(h.max(20.0) as i32)
                    .css_classes(css_classes)
                    .tooltip_text(&tooltip)
                    .build();

                self.timeline_fixed.put(&widget, x, y);
            }
        }
    }
}

const HOUR_HEIGHT: f64 = 30.0;

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
    format!("{weekday}, {}", date.format("%d.%m.%Y"))
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
                format!("box-shadow: 0 0 0 2px {color}; font-weight: 600;")
            } else {
                "border: none; font-weight: 500;".to_string()
            };
            let (radius, padding) = if is_all_day {
                ("4px", "3px 8px")
            } else {
                ("4px", "2px 6px")
            };
            let css = format!(
                "label.{class_name} {{ background-color: {bg}; color: {fg}; {border_rule} border-radius: {radius}; padding: {padding}; }}"
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
    }
}

