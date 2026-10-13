// SPDX-License-Identifier: GPL-3.0-or-later
//
// PluginsDialog — Evolution Standard-Plugins manager and configuration dialog.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::gettext;
use crate::services::plugin_service::{PluginCategory, PluginDescriptor, PluginManager};
use crate::services::settings_service::AppSettings;

pub struct PluginsDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl PluginsDialog {
    pub fn new<P: IsA<gtk::Widget>>(
        parent: Option<&P>,
        settings: Option<AppSettings>,
    ) -> Self {
        let app_settings = settings.or_else(AppSettings::try_new);

        let dialog = adw::Dialog::builder()
            .title(&gettext("Standard Plugins"))
            .content_width(620)
            .content_height(560)
            .build();

        let stored_parent = RefCell::new(parent.map(|p| p.upcast_ref::<gtk::Widget>().clone()));

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let close_btn = gtk::Button::builder().label(&gettext("Close")).build();
        let dlg_close = dialog.downgrade();
        close_btn.connect_clicked(move |_| {
            if let Some(w) = dlg_close.upgrade() {
                w.close();
            }
        });
        header.pack_end(&close_btn);
        toolbar.add_top_bar(&header);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .build();

        let pref_page = adw::PreferencesPage::new();

        let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        search_box.set_margin_start(16);
        search_box.set_margin_end(16);
        search_box.set_margin_top(12);
        search_box.set_margin_bottom(8);

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(&gettext("Filter plugins…"))
            .hexpand(true)
            .build();
        search_box.append(&search_entry);

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&search_box);

        let all_plugins = PluginManager::all_plugins();
        let app_settings_rc = Rc::new(app_settings);

        let categories = [
            (PluginCategory::Mail, gettext("Email Plugins")),
            (PluginCategory::Junk, gettext("Spam & Junk Filter")),
            (PluginCategory::Calendar, gettext("Calendar & Meeting Invitations")),
            (PluginCategory::Utilities, gettext("Utilities & Editors")),
        ];

        let mut groups: Vec<(adw::PreferencesGroup, Vec<(&'static PluginDescriptor, adw::SwitchRow)>)> = Vec::new();

        for (cat, cat_title) in categories {
            let group = adw::PreferencesGroup::builder()
                .title(&cat_title)
                .build();

            let mut row_entries = Vec::new();
            let cat_plugins: Vec<_> = all_plugins.iter().filter(|p| p.category == cat).collect();

            for plugin in cat_plugins {
                let row = adw::SwitchRow::builder()
                    .title(plugin.name)
                    .subtitle(plugin.description)
                    .build();

                let icon_name = match cat {
                    PluginCategory::Mail => "mail-message-new-symbolic",
                    PluginCategory::Security => "channel-secure-symbolic",
                    PluginCategory::Junk => "mail-mark-junk-symbolic",
                    PluginCategory::Calendar => "x-office-calendar-symbolic",
                    PluginCategory::Utilities => "applications-utilities-symbolic",
                };
                row.add_prefix(&gtk::Image::from_icon_name(icon_name));

                if let Some(settings) = app_settings_rc.as_ref() {
                    row.set_active(settings.is_plugin_enabled(plugin.id));

                    let plugin_id = plugin.id;
                    let settings_clone = settings.clone();
                    row.connect_active_notify(move |switch_row| {
                        settings_clone.set_plugin_enabled(plugin_id, switch_row.is_active());
                    });
                }

                if plugin.is_configurable {
                    let config_btn = gtk::Button::builder()
                        .icon_name("emblem-system-symbolic")
                        .tooltip_text(&format!("{} {}…", gettext("Configure"), plugin.name))
                        .valign(gtk::Align::Center)
                        .css_classes(vec!["flat"])
                        .build();

                    let dlg_config = dialog.downgrade();
                    let settings_config = app_settings_rc.clone();
                    let plugin_id = plugin.id;
                    config_btn.connect_clicked(move |_| {
                        if let Some(parent) = dlg_config.upgrade() {
                            present_plugin_config_dialog(&parent, plugin_id, settings_config.as_ref().clone());
                        }
                    });
                    row.add_suffix(&config_btn);
                }

                group.add(&row);
                row_entries.push((plugin, row));
            }

            pref_page.add(&group);
            groups.push((group, row_entries));
        }

        // Search filtering logic
        let groups_rc = Rc::new(RefCell::new(groups));
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_lowercase();
            let mut groups = groups_rc.borrow_mut();
            for (group, rows) in groups.iter_mut() {
                let mut visible_count = 0;
                for (plugin, row) in rows {
                    let matches = query.is_empty()
                        || plugin.name.to_lowercase().contains(&query)
                        || plugin.description.to_lowercase().contains(&query)
                        || plugin.id.to_lowercase().contains(&query);
                    row.set_visible(matches);
                    if matches {
                        visible_count += 1;
                    }
                }
                group.set_visible(visible_count > 0);
            }
        });

        scrolled.set_child(Some(&pref_page));
        main_box.append(&scrolled);
        toolbar.set_content(Some(&main_box));
        dialog.set_child(Some(&toolbar));

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
    }
}

pub fn present_plugins_dialog<P: IsA<gtk::Widget>>(
    parent: Option<&P>,
    settings: Option<AppSettings>,
) {
    let dialog = PluginsDialog::new(parent, settings);
    dialog.present();
}

fn present_plugin_config_dialog<P: IsA<gtk::Widget>>(
    parent: &P,
    plugin_id: &str,
    settings: Option<AppSettings>,
) {
    let dialog = adw::Dialog::builder()
        .content_width(500)
        .content_height(420)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();

    let close_btn = gtk::Button::builder().label(&gettext("Done")).build();
    let dlg_close = dialog.downgrade();
    close_btn.connect_clicked(move |_| {
        if let Some(w) = dlg_close.upgrade() {
            w.close();
        }
    });
    header.pack_end(&close_btn);
    toolbar.add_top_bar(&header);

    let pref_page = adw::PreferencesPage::new();

    match plugin_id {
        "org.gnome.evolution.plugin.attachment-reminder" => {
            dialog.set_title(&gettext("Configure Attachment Reminder"));
            let group = adw::PreferencesGroup::builder()
                .title(&gettext("Reminder Keywords"))
                .description(&gettext(
                    "Warn before sending if any of these phrases appear in the message body but no attachment is present.",
                ))
                .build();

            let row_enabled = adw::SwitchRow::builder()
                .title(&gettext("Reminder Enabled"))
                .subtitle(&gettext("Check for missing attachments when sending"))
                .build();
            row_enabled.add_prefix(&gtk::Image::from_icon_name("mail-attachment-symbolic"));

            if let Some(s) = &settings {
                row_enabled.set_active(s.plugin_attachment_reminder_enabled());
                let s_clone = s.clone();
                row_enabled.connect_active_notify(move |row| {
                    s_clone.set_plugin_attachment_reminder_enabled(row.is_active());
                });
            }
            group.add(&row_enabled);

            let row_keywords = adw::EntryRow::builder()
                .title(&gettext("Keywords (comma-separated)"))
                .text(
                    &settings
                        .as_ref()
                        .map(|s| s.plugin_attachment_reminder_keywords().join(", "))
                        .unwrap_or_else(|| "attachment, attached, anhang, anbei, enclosed".to_string()),
                )
                .build();
            row_keywords.add_prefix(&gtk::Image::from_icon_name("edit-find-symbolic"));

            if let Some(s) = &settings {
                let s_clone = s.clone();
                row_keywords.connect_apply(move |entry| {
                    let kws: Vec<String> = entry
                        .text()
                        .split(',')
                        .map(str::trim)
                        .filter(|k| !k.is_empty())
                        .map(String::from)
                        .collect();
                    s_clone.set_plugin_attachment_reminder_keywords(&kws);
                });
            }
            group.add(&row_keywords);
            pref_page.add(&group);
        }
        "org.gnome.evolution.plugin.mail-notification" => {
            dialog.set_title(&gettext("Configure Email Notifications"));
            let group = adw::PreferencesGroup::builder()
                .title(&gettext("Notification Behavior"))
                .description(&gettext("Configure notifications and alert sounds like Evolution"))
                .build();

            let row_unread = adw::SwitchRow::builder()
                .title(&gettext("Desktop Notification"))
                .subtitle(&gettext("Show notification popup when new unread messages arrive"))
                .build();
            row_unread.add_prefix(&gtk::Image::from_icon_name("preferences-system-notifications-symbolic"));

            let row_inbox_only = adw::SwitchRow::builder()
                .title(&gettext("Notify for Inbox only"))
                .subtitle(&gettext("Ignore messages received in other folders"))
                .build();
            row_inbox_only.add_prefix(&gtk::Image::from_icon_name("mail-read-symbolic"));

            let row_sound = adw::SwitchRow::builder()
                .title(&gettext("Play Sound"))
                .subtitle(&gettext("Play an audio alert when new mail arrives (30-second throttle)"))
                .build();
            row_sound.add_prefix(&gtk::Image::from_icon_name("audio-volume-high-symbolic"));

            let row_sound_beep = adw::SwitchRow::builder()
                .title(&gettext("Use System Beep"))
                .subtitle(&gettext("Play the system alert tone instead of a sound file or sound theme"))
                .build();
            row_sound_beep.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));

            let row_sound_file = adw::EntryRow::builder()
                .title(&gettext("Sound file (optional)"))
                .text(&settings.as_ref().map(|s| s.mail_notification_sound_file()).unwrap_or_default())
                .build();
            row_sound_file.add_prefix(&gtk::Image::from_icon_name("folder-music-symbolic"));

            if let Some(s) = &settings {
                row_unread.set_active(s.mail_notification_enabled());
                let s_unread = s.clone();
                row_unread.connect_active_notify(move |row| {
                    s_unread.set_mail_notification_enabled(row.is_active());
                });

                row_inbox_only.set_active(s.mail_notification_only_inbox());
                let s_inbox = s.clone();
                row_inbox_only.connect_active_notify(move |row| {
                    s_inbox.set_mail_notification_only_inbox(row.is_active());
                });

                row_sound.set_active(s.mail_notification_sound_enabled());
                let s_sound = s.clone();
                row_sound.connect_active_notify(move |row| {
                    s_sound.set_mail_notification_sound_enabled(row.is_active());
                });

                row_sound_beep.set_active(s.mail_notification_sound_beep());
                let s_beep = s.clone();
                row_sound_beep.connect_active_notify(move |row| {
                    s_beep.set_mail_notification_sound_beep(row.is_active());
                    if row.is_active() {
                        s_beep.set_mail_notification_sound_play_file(false);
                        s_beep.set_mail_notification_sound_use_theme(false);
                    } else {
                        s_beep.set_mail_notification_sound_use_theme(true);
                    }
                });

                let s_file = s.clone();
                row_sound_file.connect_apply(move |entry| {
                    let path = entry.text().trim().to_string();
                    let has_path = !path.is_empty();
                    s_file.set_mail_notification_sound_file(&path);
                    s_file.set_mail_notification_sound_play_file(has_path);
                    if has_path {
                        s_file.set_mail_notification_sound_beep(false);
                        s_file.set_mail_notification_sound_use_theme(false);
                    }
                });
            }

            group.add(&row_unread);
            group.add(&row_inbox_only);
            group.add(&row_sound);
            group.add(&row_sound_beep);
            group.add(&row_sound_file);
            pref_page.add(&group);
        }
        "org.gnome.evolution.plugin.external-editor" => {
            dialog.set_title(&gettext("Configure External Editor"));
            let group = adw::PreferencesGroup::builder()
                .title(&gettext("Editor Command"))
                .description(&gettext(
                    "Specify the command to launch the external text editor (%s will be replaced with the file path).",
                ))
                .build();

            let row_cmd = adw::EntryRow::builder()
                .title(&gettext("Command line"))
                .text(
                    &settings
                        .as_ref()
                        .map(|s| s.plugin_external_editor_command())
                        .unwrap_or_else(|| "gnome-text-editor %s".to_string()),
                )
                .build();
            row_cmd.add_prefix(&gtk::Image::from_icon_name("system-run-symbolic"));

            if let Some(s) = &settings {
                let s_clone = s.clone();
                row_cmd.connect_apply(move |entry| {
                    s_clone.set_plugin_external_editor_command(&entry.text());
                });
            }
            group.add(&row_cmd);
            pref_page.add(&group);
        }
        _ => {
            dialog.set_title(&gettext("Plugin Information"));
            let group = adw::PreferencesGroup::builder()
                .title(&gettext("Default Settings"))
                .description(&gettext("No additional configuration is required for this plugin."))
                .build();
            pref_page.add(&group);
        }
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&pref_page)
        .build();

    toolbar.set_content(Some(&scrolled));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}
