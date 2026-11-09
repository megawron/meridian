// SPDX-License-Identifier: GPL-3.0-or-later
//
// OutOfOfficeDialog — Out of Office & Auto-Responder Dialog (Exchange EWS / IMAP).
//
// Features:
// - Status Toggle (Enable/disable automatic replies)
// - Time Range (Start & end date)
// - Messages for internal & external senders via RichTextField

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::Cell;
use std::rc::Rc;
use std::thread;

use crate::i18n::gettext;
use crate::services::AppSettings;
use crate::services::account_config_service::{AccountConfigStore, AccountProtocol};
use crate::services::exchange_service::ExchangeService;
use crate::services::out_of_office_service::{OutOfOfficeSettings, OutOfOfficeStore};
use crate::views::rich_text_field::{RichTextField, is_html_content};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct OutOfOfficeDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for OutOfOfficeDialog {
        const NAME: &'static str = "MeridianOutOfOfficeDialog";
        type Type = super::OutOfOfficeDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for OutOfOfficeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_content_width(640);
            obj.set_content_height(560);
            obj.set_follows_content_size(false);
            obj.build_ui();
        }
    }

    impl WidgetImpl for OutOfOfficeDialog {}
    impl AdwDialogImpl for OutOfOfficeDialog {}
}

glib::wrapper! {
    pub struct OutOfOfficeDialog(ObjectSubclass<imp::OutOfOfficeDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OutOfOfficeDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", gettext("Out of Office (Auto-Responder)"))
            .property("content-width", 640)
            .property("content-height", 560)
            .property("follows-content-size", false)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();

        let title_widget = adw::WindowTitle::new(&gettext("Out of Office (Auto-Responder)"), "");
        header_bar.set_title_widget(Some(&title_widget));

        let btn_cancel = gtk::Button::builder()
            .label(gettext("Cancel"))
            .build();
        let dialog_for_cancel = self.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header_bar.pack_start(&btn_cancel);

        let btn_save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();

        let graph_service = graph_service_for_configured_account();
        let saved_settings = OutOfOfficeStore::default().load();

        // Group 1: Untitled
        let group_schedule = adw::PreferencesGroup::new();

        let row_enabled = adw::SwitchRow::builder()
            .title(gettext("Send Automatic Replies"))
            .active(saved_settings.enabled)
            .build();
        group_schedule.add(&row_enabled);

        let expander_schedule = adw::ExpanderRow::builder()
            .title(gettext("Only During a Time Range"))
            .show_enable_switch(true)
            .enable_expansion(saved_settings.use_schedule)
            .build();

        let (action_start, row_start) = create_date_picker_row(&gettext("Start Date"), None);
        expander_schedule.add_row(&action_start);

        let (action_end, row_end) = create_date_picker_row(&gettext("End Date"), None);
        expander_schedule.add_row(&action_end);

        row_start.set_text(&saved_settings.start_date);
        row_end.set_text(&saved_settings.end_date);

        let is_sched_enabled = saved_settings.use_schedule;
        action_start.set_sensitive(is_sched_enabled);
        action_end.set_sensitive(is_sched_enabled);
        row_start.set_sensitive(is_sched_enabled);
        row_end.set_sensitive(is_sched_enabled);

        let action_start_for_schedule = action_start.clone();
        let action_end_for_schedule = action_end.clone();
        let row_start_for_schedule = row_start.clone();
        let row_end_for_schedule = row_end.clone();
        expander_schedule.connect_enable_expansion_notify(move |row| {
            let active = row.enables_expansion();
            action_start_for_schedule.set_sensitive(active);
            action_end_for_schedule.set_sensitive(active);
            row_start_for_schedule.set_sensitive(active);
            row_end_for_schedule.set_sensitive(active);
        });

        group_schedule.add(&expander_schedule);
        page.add(&group_schedule);

        // Group 2: Inside My Organization
        let group_internal = adw::PreferencesGroup::builder()
            .title(gettext("Inside My Organization"))
            .build();

        let rich_internal = RichTextField::new(false, 140);
        group_internal.add(&rich_internal);
        page.add(&group_internal);

        // Group 3: Outside My Organization
        let group_external = adw::PreferencesGroup::builder()
            .title(gettext("Outside My Organization"))
            .build();

        let rich_external = RichTextField::new(false, 140);
        group_external.add(&rich_external);
        page.add(&group_external);

        // Sensitivity: Groups 2 and 3 become insensitive while the main switch is off
        group_internal.set_sensitive(saved_settings.enabled);
        group_external.set_sensitive(saved_settings.enabled);

        let group_internal_for_toggle = group_internal.clone();
        let group_external_for_toggle = group_external.clone();
        row_enabled.connect_active_notify(move |row| {
            let active = row.is_active();
            group_internal_for_toggle.set_sensitive(active);
            group_external_for_toggle.set_sensitive(active);
        });

        // Load initial messages
        if is_html_content(&saved_settings.internal_message) {
            rich_internal.set_html(&saved_settings.internal_message);
        } else {
            rich_internal.set_plain_text(&saved_settings.internal_message);
        }

        if is_html_content(&saved_settings.external_message) {
            rich_external.set_html(&saved_settings.external_message);
        } else {
            rich_external.set_plain_text(&saved_settings.external_message);
        }

        if let Some(service) = graph_service.clone() {
            load_graph_settings(
                self,
                service,
                &row_enabled,
                &expander_schedule,
                &row_start,
                &row_end,
                &rich_internal,
                &rich_external,
            );
        }

        let dialog_for_save = self.clone();
        let row_enabled_for_save = row_enabled.clone();
        let expander_schedule_for_save = expander_schedule.clone();
        let row_start_for_save = row_start.clone();
        let row_end_for_save = row_end.clone();
        let rich_internal_for_save = rich_internal.clone();
        let rich_external_for_save = rich_external.clone();
        let graph_service_for_save = graph_service.clone();
        let save_button_for_save = btn_save.clone();

        btn_save.connect_clicked(move |_| {
            let internal_message = if rich_internal_for_save.plain_text().trim().is_empty() {
                String::new()
            } else {
                rich_internal_for_save.html()
            };
            let external_message = if rich_external_for_save.plain_text().trim().is_empty() {
                String::new()
            } else {
                rich_external_for_save.html()
            };

            let settings = OutOfOfficeSettings {
                enabled: row_enabled_for_save.is_active(),
                use_schedule: expander_schedule_for_save.enables_expansion(),
                start_date: row_start_for_save.text().trim().to_string(),
                end_date: row_end_for_save.text().trim().to_string(),
                internal_message,
                external_message,
            };

            if let Err(error) = settings.validate() {
                show_dialog_toast(&dialog_for_save, &error);
                return;
            }

            match OutOfOfficeStore::default().save(&settings) {
                Ok(()) => {
                    let Some(service) = graph_service_for_save.clone() else {
                        show_dialog_toast(
                            &dialog_for_save,
                            &gettext("Out-of-office settings saved locally; server delivery configured per account"),
                        );
                        dialog_for_save.close();
                        return;
                    };

                    save_button_for_save.set_sensitive(false);
                    let settings_for_server = settings.clone();
                    let dialog_weak = dialog_for_save.downgrade();
                    let save_button = save_button_for_save.clone();
                    let (sender, receiver) = async_channel::bounded(1);
                    thread::spawn(move || {
                        let result = service.set_out_of_office_sync(&settings_for_server);
                        let _ = sender.send_blocking(result);
                    });
                    glib::spawn_future_local(async move {
                        let result = receiver.recv().await;
                        let Some(dialog) = dialog_weak.upgrade() else {
                            return;
                        };
                        save_button.set_sensitive(true);
                        match result {
                            Ok(Ok(server_settings)) => {
                                if let Err(error) = OutOfOfficeStore::default().save(&server_settings)
                                {
                                    tracing::warn!(%error, "Could not persist Graph OOF response");
                                }
                                show_dialog_toast(
                                    &dialog,
                                    &gettext("Out-of-office settings saved locally and on Exchange server"),
                                );
                                dialog.close();
                            }
                            Ok(Err(error)) => {
                                show_dialog_toast(
                                    &dialog,
                                    &format!("{}: {error}", gettext("Saved locally, server update failed")),
                                );
                            }
                            Err(error) => {
                                show_dialog_toast(
                                    &dialog,
                                    &format!("{}: {error}", gettext("Server update cancelled")),
                                );
                            }
                        }
                    });
                }
                Err(error) => show_dialog_toast(
                    &dialog_for_save,
                    &format!("{}: {error}", gettext("Could not save out-of-office settings")),
                ),
            }
        });

        toolbar_view.set_content(Some(&page));
        self.set_child(Some(&toolbar_view));
    }
}

fn graph_service_for_configured_account() -> Option<ExchangeService> {
    let accounts = AccountConfigStore::default().load();
    let preferred_id = AppSettings::try_new()
        .map(|settings| settings.default_account_id())
        .filter(|id| !id.trim().is_empty());
    let is_exchange = |profile: &&crate::services::account_config_service::AccountProfile| {
        profile.enabled && profile.protocol == AccountProtocol::Exchange
    };
    let profile = preferred_id
        .as_deref()
        .and_then(|id| {
            accounts
                .iter()
                .find(|profile| is_exchange(profile) && profile.id == id)
        })
        .or_else(|| accounts.iter().find(is_exchange))?;
    ExchangeService::from_environment(&profile.email)
}

fn load_graph_settings(
    dialog: &OutOfOfficeDialog,
    service: ExchangeService,
    row_enabled: &adw::SwitchRow,
    expander_schedule: &adw::ExpanderRow,
    row_start: &adw::EntryRow,
    row_end: &adw::EntryRow,
    rich_internal: &RichTextField,
    rich_external: &RichTextField,
) {
    let dialog_weak = dialog.downgrade();
    let row_enabled = row_enabled.clone();
    let expander_schedule = expander_schedule.clone();
    let row_start = row_start.clone();
    let row_end = row_end.clone();
    let rich_internal = rich_internal.clone();
    let rich_external = rich_external.clone();
    let (sender, receiver) = async_channel::bounded(1);
    thread::spawn(move || {
        let result = service.get_out_of_office_sync();
        let _ = sender.send_blocking(result);
    });
    glib::spawn_future_local(async move {
        let result = receiver.recv().await;
        let Some(dialog) = dialog_weak.upgrade() else {
            return;
        };
        match result {
            Ok(Ok(settings)) => {
                row_enabled.set_active(settings.enabled);
                expander_schedule.set_enable_expansion(settings.use_schedule);
                row_start.set_text(&settings.start_date);
                row_end.set_text(&settings.end_date);
                if is_html_content(&settings.internal_message) {
                    rich_internal.set_html(&settings.internal_message);
                } else {
                    rich_internal.set_plain_text(&settings.internal_message);
                }
                if is_html_content(&settings.external_message) {
                    rich_external.set_html(&settings.external_message);
                } else {
                    rich_external.set_plain_text(&settings.external_message);
                }
                show_dialog_toast(&dialog, &gettext("Microsoft Graph: Server status loaded"));
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "Could not load Graph out-of-office settings");
            }
            Err(error) => {
                tracing::warn!(%error, "Graph out-of-office load was cancelled");
            }
        }
    });
}

fn parse_iso_date(iso: &str) -> Option<(i32, i32, i32)> {
    let parts: Vec<&str> = iso.trim().split('-').collect();
    if parts.len() == 3 {
        let year: i32 = parts[0].parse().ok()?;
        let month: i32 = parts[1].parse().ok()?;
        let day: i32 = parts[2].parse().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some((year, month, day));
        }
    }
    None
}

fn create_date_picker_row(
    title: &str,
    initial_iso: Option<&str>,
) -> (adw::ActionRow, adw::EntryRow) {
    let iso_entry = adw::EntryRow::new();

    let action_row = adw::ActionRow::builder()
        .title(title)
        .activatable(false)
        .build();

    let suffix_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();

    let menu_button = gtk::MenuButton::builder()
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .build();

    let btn_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let btn_label = gtk::Label::builder()
        .label(gettext("Select date…"))
        .css_classes(["dim-label"])
        .build();
    btn_box.append(&btn_label);
    menu_button.set_child(Some(&btn_box));

    let btn_clear = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(gettext("Clear date"))
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();

    let popover = gtk::Popover::new();
    let popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();

    let calendar = gtk::Calendar::new();
    popover_box.append(&calendar);

    let popover_btn_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .homogeneous(true)
        .build();

    let btn_today = gtk::Button::builder()
        .label(gettext("Today"))
        .css_classes(["flat"])
        .build();

    let btn_popover_clear = gtk::Button::builder()
        .label(gettext("Clear"))
        .css_classes(["flat"])
        .build();

    popover_btn_box.append(&btn_today);
    popover_btn_box.append(&btn_popover_clear);
    popover_box.append(&popover_btn_box);
    popover.set_child(Some(&popover_box));
    menu_button.set_popover(Some(&popover));

    suffix_box.append(&menu_button);
    suffix_box.append(&btn_clear);
    action_row.add_suffix(&suffix_box);

    let is_updating = Rc::new(Cell::new(false));

    let update_ui_from_iso = {
        let btn_label = btn_label.clone();
        let btn_clear = btn_clear.clone();
        let calendar = calendar.clone();
        move |iso: &str| {
            if let Some((year, month, day)) = parse_iso_date(iso) {
                btn_label.set_text(&format!("{:02}.{:02}.{:04}", day, month, year));
                btn_label.remove_css_class("dim-label");
                btn_clear.set_visible(true);
                let tz = glib::TimeZone::local();
                if let Ok(dt) = glib::DateTime::new(&tz, year, month, day, 0, 0, 0.0) {
                    calendar.select_day(&dt);
                }
            } else {
                btn_label.set_text(&gettext("Select date…"));
                btn_label.add_css_class("dim-label");
                btn_clear.set_visible(false);
            }
        }
    };

    {
        let iso_entry = iso_entry.clone();
        let popover = popover.clone();
        let is_updating = is_updating.clone();
        calendar.connect_day_selected(move |cal| {
            if is_updating.get() {
                return;
            }
            let dt = cal.date();
            let formatted = format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day_of_month());
            is_updating.set(true);
            iso_entry.set_text(&formatted);
            is_updating.set(false);
            popover.popdown();
        });
    }

    {
        let update_ui = update_ui_from_iso.clone();
        let is_updating = is_updating.clone();
        iso_entry.connect_changed(move |entry| {
            if is_updating.get() {
                return;
            }
            is_updating.set(true);
            update_ui(&entry.text());
            is_updating.set(false);
        });
    }

    {
        let iso_entry = iso_entry.clone();
        btn_clear.connect_clicked(move |_| {
            iso_entry.set_text("");
        });
    }

    {
        let iso_entry = iso_entry.clone();
        let popover = popover.clone();
        btn_today.connect_clicked(move |_| {
            if let Ok(now) = glib::DateTime::now_local() {
                let formatted = format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day_of_month());
                iso_entry.set_text(&formatted);
            }
            popover.popdown();
        });
    }

    {
        let iso_entry = iso_entry.clone();
        let popover = popover.clone();
        btn_popover_clear.connect_clicked(move |_| {
            iso_entry.set_text("");
            popover.popdown();
        });
    }

    if let Some(iso) = initial_iso {
        iso_entry.set_text(iso);
        update_ui_from_iso(iso);
    }

    (action_row, iso_entry)
}

fn show_dialog_toast(dialog: &OutOfOfficeDialog, message: &str) {
    let toast = adw::Toast::new(message);
    if let Some(window) = dialog.root().and_downcast::<crate::window::MeridianWindow>() {
        window.add_toast(&toast);
        return;
    }
    tracing::info!(%message, "Out-of-office dialog result");
}

impl Default for OutOfOfficeDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oof_dialog_creation_and_properties() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = OutOfOfficeDialog::new();
        assert_eq!(dialog.content_width(), 640);
        assert_eq!(dialog.content_height(), 560);
        assert!(!dialog.follows_content_size());
    }

    #[test]
    fn test_parse_iso_date() {
        assert_eq!(parse_iso_date("2026-10-29"), Some((2026, 10, 29)));
        assert_eq!(parse_iso_date("invalid"), None);
        assert_eq!(parse_iso_date("2026-13-01"), None);
        assert_eq!(parse_iso_date("2026-00-01"), None);
    }
}
