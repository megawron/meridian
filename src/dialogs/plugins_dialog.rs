// SPDX-License-Identifier: GPL-3.0-or-later
//
// PluginsDialog — Evolution Standard-Plugins manager and configuration dialog.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

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
            .title("Standard-Plugins")
            .content_width(620)
            .content_height(560)
            .build();

        let stored_parent = RefCell::new(parent.map(|p| p.upcast_ref::<gtk::Widget>().clone()));

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let close_btn = gtk::Button::builder().label("Schließen").build();
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
            .placeholder_text("Plugins filtern…")
            .hexpand(true)
            .build();
        search_box.append(&search_entry);

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&search_box);

        let all_plugins = PluginManager::all_plugins();
        let app_settings_rc = Rc::new(app_settings);

        let categories = [
            (PluginCategory::Mail, "E-Mail-Plugins"),
            (PluginCategory::Junk, "Spam- & Junk-Filter"),
            (PluginCategory::Calendar, "Kalender & Termineinladungen"),
            (PluginCategory::Utilities, "Hilfsprogramme & Editoren"),
        ];

        let mut groups: Vec<(adw::PreferencesGroup, Vec<(&'static PluginDescriptor, adw::SwitchRow)>)> = Vec::new();

        for (cat, cat_title) in categories {
            let group = adw::PreferencesGroup::builder()
                .title(cat_title)
                .build();

            let mut row_entries = Vec::new();
            let cat_plugins: Vec<_> = all_plugins.iter().filter(|p| p.category == cat).collect();

            for plugin in cat_plugins {
                let row = adw::SwitchRow::builder()
                    .title(plugin.name)
                    .subtitle(plugin.description)
                    .build();

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
                        .tooltip_text(format!("{} konfigurieren…", plugin.name))
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

    let close_btn = gtk::Button::builder().label("Fertig").build();
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
            dialog.set_title("Anhang-Erinnerung konfigurieren");
            let group = adw::PreferencesGroup::builder()
                .title("Erinnerungs-Schlüsselwörter")
                .description("Wenn eine dieser Phrasen im Text vorkommt, aber kein Anhang vorhanden ist, warnt Evolution vor dem Senden.")
                .build();

            let row_enabled = adw::SwitchRow::builder()
                .title("Erinnerung aktiv")
                .subtitle("Beim Absenden auf vergessene Anhänge prüfen")
                .build();

            if let Some(s) = &settings {
                row_enabled.set_active(s.plugin_attachment_reminder_enabled());
                let s_clone = s.clone();
                row_enabled.connect_active_notify(move |row| {
                    s_clone.set_plugin_attachment_reminder_enabled(row.is_active());
                });
            }
            group.add(&row_enabled);

            let row_keywords = adw::EntryRow::builder()
                .title("Schlüsselwörter (kommagetrennt)")
                .text(
                    &settings
                        .as_ref()
                        .map(|s| s.plugin_attachment_reminder_keywords().join(", "))
                        .unwrap_or_else(|| "anhang, angehängt, attached, attachment, enclosed, anbei, anlage".to_string()),
                )
                .build();

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
            window.set_title(Some("E-Mail-Benachrichtigungen konfigurieren"));
            let group = adw::PreferencesGroup::builder()
                .title("Benachrichtigungsverhalten")
                .build();

            let row_unread = adw::SwitchRow::builder()
                .title("Desktop-Benachrichtigung")
                .subtitle("Popup-Banner bei neuen ungelesenen Nachrichten anzeigen")
                .build();

            let row_sound = adw::SwitchRow::builder()
                .title("Klang abspielen")
                .subtitle("Audiosignal bei Nachrichteneingang")
                .build();

            let row_sound_file = adw::EntryRow::builder()
                .title("Klangdatei (optional)")
                .text(&settings.as_ref().map(|s| s.plugin_mail_notification_sound_file()).unwrap_or_default())
                .build();

            if let Some(s) = &settings {
                row_unread.set_active(s.plugin_mail_notification_notify_unread());
                let s_unread = s.clone();
                row_unread.connect_active_notify(move |row| {
                    s_unread.set_plugin_mail_notification_notify_unread(row.is_active());
                });

                row_sound.set_active(s.plugin_mail_notification_sound());
                let s_sound = s.clone();
                row_sound.connect_active_notify(move |row| {
                    s_sound.set_plugin_mail_notification_sound(row.is_active());
                });

                let s_file = s.clone();
                row_sound_file.connect_apply(move |entry| {
                    s_file.set_plugin_mail_notification_sound_file(&entry.text());
                });
            }

            group.add(&row_unread);
            group.add(&row_sound);
            group.add(&row_sound_file);
            pref_page.add(&group);
        }
        "org.gnome.evolution.plugin.external-editor" => {
            window.set_title(Some("Externen Editor konfigurieren"));
            let group = adw::PreferencesGroup::builder()
                .title("Editor-Befehl")
                .description("Geben Sie den Befehl zum Starten des externen Texteditors an (%s wird durch den Dateipfad ersetzt).")
                .build();

            let row_cmd = adw::EntryRow::builder()
                .title("Befehlszeile")
                .text(
                    &settings
                        .as_ref()
                        .map(|s| s.plugin_external_editor_command())
                        .unwrap_or_else(|| "gnome-text-editor %s".to_string()),
                )
                .build();

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
            dialog.set_title("Plugin-Informationen");
            let group = adw::PreferencesGroup::builder()
                .title("Standardeinstellungen")
                .description("Für dieses Plugin sind keine zusätzlichen Parameter erforderlich.")
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
