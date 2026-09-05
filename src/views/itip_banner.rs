// SPDX-License-Identifier: GPL-3.0-or-later
//
// ItipBanner — Interactive GNOME Evolution-parity iTIP/iCalendar invitation banner.
//
// Positioned between message headers and the email body (matching Evolution's
// itip-view.c), this banner displays meeting details and interactive actions:
// Accept, Tentative, Decline, Cancel, Calendar target selection, and
// Open-in-Calendar navigation.

use adw::prelude::*;
use chrono::NaiveDate;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::ecal_service::{ECalComponentType, ECalService};
use crate::services::itip_service::{
    ItipInvitation, ItipMethod, ItipReplyStatus, find_user_status, format_recurrence_summary,
    update_attendee_partstat,
};

#[derive(Clone)]
pub struct ItipBanner {
    revealer: gtk::Revealer,
    banner_box: gtk::Box,
    icon: gtk::Image,
    lbl_title: gtk::Label,
    lbl_method: gtk::Label,
    lbl_status_badge: gtk::Label,
    lbl_time: gtk::Label,
    lbl_location: gtk::Label,
    lbl_organizer: gtk::Label,
    lbl_attendees: gtk::Label,
    lbl_recurrence: gtk::Label,
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
    action_box_request: gtk::Box,
    action_box_cancel: gtk::Box,
    action_box_reply: gtk::Box,
    action_box_publish: gtk::Box,
    current_invitation: Rc<RefCell<Option<ItipInvitation>>>,
    on_calendar_navigate: Rc<RefCell<Option<Box<dyn Fn(NaiveDate)>>>>,
    on_action_toast: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
    on_reply_draft_saved: Rc<RefCell<Option<Box<dyn Fn(&ItipInvitation, ItipReplyStatus)>>>>,
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
            .build();

        let banner_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(vec!["itip-banner", "card"])
            .margin_start(16)
            .margin_end(16)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        // ── Header Bar ───────────────────────────────────────────────────────
        let header_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .valign(gtk::Align::Center)
            .build();

        let icon = gtk::Image::builder()
            .icon_name("x-office-calendar-symbolic")
            .pixel_size(32)
            .css_classes(vec!["accent"])
            .valign(gtk::Align::Center)
            .build();
        header_row.append(&icon);

        let title_vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();

        let lbl_method = gtk::Label::builder()
            .label("Besprechungseinladung")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .build();
        title_vbox.append(&lbl_method);

        let lbl_title = gtk::Label::builder()
            .label("Termin")
            .css_classes(vec!["title-3"])
            .halign(gtk::Align::Start)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        title_vbox.append(&lbl_title);
        header_row.append(&title_vbox);

        let lbl_status_badge = gtk::Label::builder()
            .label("Antwort ausstehend")
            .css_classes(vec!["itip-status-badge", "pending"])
            .valign(gtk::Align::Center)
            .build();
        header_row.append(&lbl_status_badge);

        banner_box.append(&header_row);

        // ── Separator ────────────────────────────────────────────────────────
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        banner_box.append(&sep);

        // ── Details Grid ─────────────────────────────────────────────────────
        let details_vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();

        // Time / Date row
        let time_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let time_icon = gtk::Image::builder()
            .icon_name("alarm-symbolic")
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_time = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .css_classes(vec!["body"])
            .build();
        time_row.append(&time_icon);
        time_row.append(&lbl_time);
        details_vbox.append(&time_row);

        // Recurrence row
        let recurrence_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .build();
        let rec_icon = gtk::Image::builder()
            .icon_name("media-playlist-repeat-symbolic")
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_recurrence = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        recurrence_row.append(&rec_icon);
        recurrence_row.append(&lbl_recurrence);
        details_vbox.append(&recurrence_row);

        // Location row
        let location_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .build();
        let loc_icon = gtk::Image::builder()
            .icon_name("find-location-symbolic")
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_location = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(vec!["body"])
            .build();
        location_row.append(&loc_icon);
        location_row.append(&lbl_location);
        details_vbox.append(&location_row);

        // Organizer row
        let organizer_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .build();
        let org_icon = gtk::Image::builder()
            .icon_name("avatar-default-symbolic")
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_organizer = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(vec!["caption"])
            .build();
        organizer_row.append(&org_icon);
        organizer_row.append(&lbl_organizer);
        details_vbox.append(&organizer_row);

        // Attendees row
        let attendees_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .visible(false)
            .build();
        let att_icon = gtk::Image::builder()
            .icon_name("system-users-symbolic")
            .css_classes(vec!["dim-label"])
            .build();
        let lbl_attendees = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        attendees_row.append(&att_icon);
        attendees_row.append(&lbl_attendees);
        details_vbox.append(&attendees_row);

        banner_box.append(&details_vbox);

        // ── Action Footer ────────────────────────────────────────────────────
        let footer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(4)
            .build();

        // Calendar target selector
        let cal_select_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let cal_select_lbl = gtk::Label::builder()
            .label("Kalender:")
            .css_classes(vec!["caption", "dim-label"])
            .build();
        cal_select_box.append(&cal_select_lbl);

        let calendar_dropdown = gtk::DropDown::builder()
            .valign(gtk::Align::Center)
            .build();
        cal_select_box.append(&calendar_dropdown);
        footer_box.append(&cal_select_box);

        let action_spacer = gtk::Box::builder().hexpand(true).build();
        footer_box.append(&action_spacer);

        // REQUEST actions
        let action_box_request = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();

        let btn_accept = gtk::Button::builder()
            .label("Zusagen")
            .css_classes(vec!["suggested-action"])
            .tooltip_text("Termin zusagen, in den Kalender übernehmen und Antwort senden")
            .build();
        let btn_tentative = gtk::Button::builder()
            .label("Mit Vorbehalt")
            .tooltip_text("Mit Vorbehalt zusagen und in den Kalender übernehmen")
            .build();
        let btn_decline = gtk::Button::builder()
            .label("Ablehnen")
            .css_classes(vec!["destructive-action"])
            .tooltip_text("Einladung ablehnen und absagen")
            .build();

        action_box_request.append(&btn_accept);
        action_box_request.append(&btn_tentative);
        action_box_request.append(&btn_decline);
        footer_box.append(&action_box_request);

        // CANCEL actions
        let action_box_cancel = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();

        let btn_remove = gtk::Button::builder()
            .label("Aus Kalender entfernen")
            .css_classes(vec!["destructive-action"])
            .tooltip_text("Abgesagten Termin aus Ihrem Kalender entfernen")
            .build();
        action_box_cancel.append(&btn_remove);
        footer_box.append(&action_box_cancel);

        // REPLY actions
        let action_box_reply = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();

        let btn_update = gtk::Button::builder()
            .label("Kalender aktualisieren")
            .css_classes(vec!["suggested-action"])
            .tooltip_text("Teilnehmerstatus im bestehenden Kalendereintrag aktualisieren")
            .build();
        action_box_reply.append(&btn_update);
        footer_box.append(&action_box_reply);

        // PUBLISH actions
        let action_box_publish = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();

        let btn_import = gtk::Button::builder()
            .label("In Kalender eintragen")
            .css_classes(vec!["suggested-action"])
            .tooltip_text("Veröffentlichten Termin in den Kalender übernehmen")
            .build();
        action_box_publish.append(&btn_import);
        footer_box.append(&action_box_publish);

        // Open in Calendar button (always available on right)
        let btn_open_calendar = gtk::Button::builder()
            .label("Im Kalender öffnen")
            .icon_name("x-office-calendar-symbolic")
            .css_classes(vec!["flat"])
            .tooltip_text("Zum Kalender wechseln und Datum anzeigen")
            .valign(gtk::Align::Center)
            .build();
        footer_box.append(&btn_open_calendar);

        banner_box.append(&footer_box);
        revealer.set_child(Some(&banner_box));

        let banner = Self {
            revealer,
            banner_box,
            icon,
            lbl_title,
            lbl_method,
            lbl_status_badge,
            lbl_time,
            lbl_location,
            lbl_organizer,
            lbl_attendees,
            lbl_recurrence,
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
            action_box_request,
            action_box_cancel,
            action_box_reply,
            action_box_publish,
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

    pub fn connect_calendar_navigate<F: Fn(NaiveDate) + 'static>(&self, f: F) {
        *self.on_calendar_navigate.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_action_toast<F: Fn(String) + 'static>(&self, f: F) {
        *self.on_action_toast.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_reply_draft_saved<F: Fn(&ItipInvitation, ItipReplyStatus) + 'static>(
        &self,
        f: F,
    ) {
        *self.on_reply_draft_saved.borrow_mut() = Some(Box::new(f));
    }

    fn setup_calendar_sources(&self) {
        let mut sources = vec![("local-calendar".to_string(), "Persönlicher Kalender".to_string())];
        if let Ok(discovery) = ECalService::new().discover_source_capabilities(ECalComponentType::Event) {
            for source in discovery.sources {
                if source.writable && source.source_uid != "local-calendar" {
                    sources.push((source.source_uid, source.display_name));
                }
            }
        }

        let names: Vec<&str> = sources.iter().map(|(_, name)| name.as_str()).collect();
        let model = gtk::StringList::new(&names);
        self.calendar_dropdown.set_model(Some(&model));
        self.calendar_dropdown.set_selected(0);
        *self.calendar_sources.borrow_mut() = sources;
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
            return;
        };

        let event = &invitation.event;
        let title = if event.summary.trim().is_empty() {
            "Termin ohne Betreff"
        } else {
            event.summary.trim()
        };
        self.lbl_title.set_text(title);

        // Method description and styling
        self.banner_box.remove_css_class("itip-cancelled");
        self.banner_box.remove_css_class("itip-accepted");

        match invitation.method {
            ItipMethod::Request | ItipMethod::Add => {
                self.icon.set_icon_name(Some("x-office-calendar-symbolic"));
                self.lbl_method.set_text("Besprechungseinladung");
                self.action_box_request.set_visible(true);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
            }
            ItipMethod::Cancel => {
                self.icon.set_icon_name(Some("process-stop-symbolic"));
                self.lbl_method.set_text("Besprechung wurde abgesagt");
                self.banner_box.add_css_class("itip-cancelled");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(true);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
            }
            ItipMethod::Reply => {
                self.icon.set_icon_name(Some("mail-reply-sender-symbolic"));
                self.lbl_method.set_text("Antwort auf Besprechungseinladung");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(true);
                self.action_box_publish.set_visible(false);
            }
            ItipMethod::Publish => {
                self.icon.set_icon_name(Some("x-office-calendar-symbolic"));
                self.lbl_method.set_text("Veröffentlichter Termin");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(true);
            }
            ItipMethod::Unknown => {
                self.icon.set_icon_name(Some("x-office-calendar-symbolic"));
                self.lbl_method.set_text("Kalendereintrag");
                self.action_box_request.set_visible(false);
                self.action_box_cancel.set_visible(false);
                self.action_box_reply.set_visible(false);
                self.action_box_publish.set_visible(false);
            }
        }

        // Time / Date range
        let time_str = if event.all_day {
            format!("Ganztägig: {}", event.start)
        } else if !event.end.trim().is_empty() {
            format!("{} – {}", event.start, event.end)
        } else {
            event.start.clone()
        };
        self.lbl_time.set_text(&time_str);

        // Recurrence
        if let Some(rrule) = &invitation.recurrence {
            self.lbl_recurrence.set_text(&format_recurrence_summary(rrule));
            self.recurrence_row.set_visible(true);
        } else {
            self.recurrence_row.set_visible(false);
        }

        // Location
        if !event.location.trim().is_empty() {
            self.lbl_location.set_text(&event.location);
            self.location_row.set_visible(true);
        } else {
            self.location_row.set_visible(false);
        }

        // Organizer
        if !invitation.organizer.trim().is_empty() {
            let org = invitation.organizer.trim().trim_start_matches("mailto:");
            self.lbl_organizer.set_text(&format!("Organisator: {org}"));
            self.organizer_row.set_visible(true);
        } else {
            self.organizer_row.set_visible(false);
        }

        // Attendees
        if !invitation.attendees.is_empty() {
            let count = invitation.attendees.len();
            let text = if count == 1 {
                "1 Teilnehmer".to_string()
            } else {
                format!("{count} Teilnehmer")
            };
            self.lbl_attendees.set_text(&text);
            self.attendees_row.set_visible(true);
        } else {
            self.attendees_row.set_visible(false);
        }

        // Current user status
        let user_status = find_user_status(&invitation.raw_component, user_emails);
        self.update_status_badge(user_status);

        self.btn_accept.set_sensitive(true);
        self.btn_tentative.set_sensitive(true);
        self.btn_decline.set_sensitive(true);
        self.btn_remove.set_sensitive(true);
        self.btn_update.set_sensitive(true);
        self.btn_import.set_sensitive(true);

        self.revealer.set_reveal_child(true);
    }

    fn update_status_badge(&self, status: Option<ItipReplyStatus>) {
        self.lbl_status_badge.remove_css_class("accepted");
        self.lbl_status_badge.remove_css_class("tentative");
        self.lbl_status_badge.remove_css_class("declined");
        self.lbl_status_badge.remove_css_class("pending");

        match status {
            Some(ItipReplyStatus::Accepted) => {
                self.lbl_status_badge.set_text("Zugesagt ✓");
                self.lbl_status_badge.add_css_class("accepted");
                self.banner_box.add_css_class("itip-accepted");
            }
            Some(ItipReplyStatus::Tentative) => {
                self.lbl_status_badge.set_text("Mit Vorbehalt");
                self.lbl_status_badge.add_css_class("tentative");
            }
            Some(ItipReplyStatus::Declined) => {
                self.lbl_status_badge.set_text("Abgelehnt");
                self.lbl_status_badge.add_css_class("declined");
            }
            None => {
                self.lbl_status_badge.set_text("Antwort ausstehend");
                self.lbl_status_badge.add_css_class("pending");
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

        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let service = ECalService::new();
            let mut component = invitation_for_thread.event.component.clone();
            // Update attendee status in the component
            let user_email = invitation_for_thread.organizer.clone(); // fallback
            component = update_attendee_partstat(&component, &user_email, status);

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
                            toast(format!("Termin {status_label} und Kalender aktualisiert"));
                        }
                        if let Some(draft_cb) = this.on_reply_draft_saved.borrow().as_ref() {
                            draft_cb(&invitation, status);
                        }
                    }
                    Err(err) => {
                        this.btn_accept.set_sensitive(true);
                        this.btn_tentative.set_sensitive(true);
                        this.btn_decline.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("Fehler beim Aktualisieren: {err}"));
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
                        this.btn_remove.set_label("Aus Kalender entfernt");
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast("Termin aus dem Kalender entfernt".to_string());
                        }
                    }
                    Err(err) => {
                        this.btn_remove.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("Konnte nicht entfernt werden: {err}"));
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
                        this.btn_update.set_label("Aktualisiert ✓");
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast("Kalendereintrag erfolgreich aktualisiert".to_string());
                        }
                    }
                    Err(err) => {
                        this.btn_update.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("Fehler bei Aktualisierung: {err}"));
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
                        this.btn_import.set_label("Eingetragen ✓");
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast("Termin in Kalender eingetragen".to_string());
                        }
                    }
                    Err(err) => {
                        this.btn_import.set_sensitive(true);
                        if let Some(toast) = this.on_action_toast.borrow().as_ref() {
                            toast(format!("Fehler beim Eintragen: {err}"));
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

fn parse_start_date(raw: &str) -> NaiveDate {
    let trimmed = raw.trim();
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
    // Try YYYYMMDD
    if first_token.len() >= 8 {
        if let Ok(d) = NaiveDate::parse_from_str(&first_token[..8], "%Y%m%d") {
            return d;
        }
    }
    // Fallback: extract first 8 digits if formatted as YYYYMMDD
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        if let Ok(d) = NaiveDate::parse_from_str(&digits[0..8], "%Y%m%d") {
            return d;
        }
    }
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
