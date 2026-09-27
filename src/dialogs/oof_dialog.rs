// SPDX-License-Identifier: GPL-3.0-or-later
//
// OutOfOfficeDialog — Abwesenheitsnotiz & Auto-Responder Dialog (Exchange EWS / IMAP).
//
// Features:
// - Status Toggle (Abwesenheitsnotiz aktivieren/deaktivieren)
// - Zeitbereich (Start- & Enddatum)
// - Nachrichten für interne & externe Absender

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::thread;

use crate::services::AppSettings;
use crate::services::account_config_service::{AccountConfigStore, AccountProtocol};
use crate::services::exchange_service::ExchangeService;
use crate::services::out_of_office_service::{OutOfOfficeSettings, OutOfOfficeStore};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct OutOfOfficeDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for OutOfOfficeDialog {
        const NAME: &'static str = "EvolutionOutOfOfficeDialog";
        type Type = super::OutOfOfficeDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for OutOfOfficeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
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
            .property("title", "Abwesenheitsnotiz (Auto-Responder)")
            .property("content-width", 600)
            .property("content-height", 520)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let graph_service = graph_service_for_configured_account();
        let backend_state = gtk::Label::builder()
            .label(if graph_service.is_some() {
                "Microsoft Graph: Serverstatus wird geladen …"
            } else {
                "Lokale Konfiguration; die Serveranbindung bleibt zuständig"
            })
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .wrap(true)
            .build();
        content_box.append(&backend_state);

        // Status Group
        let group_status = adw::PreferencesGroup::builder()
            .title("Status & Zeitfenster")
            .build();

        let row_enabled = adw::SwitchRow::builder()
            .title("Abwesenheitsnotiz aktivieren")
            .subtitle(if graph_service.is_some() {
                "Serverstatus wird über Microsoft Graph synchronisiert"
            } else {
                "Konfiguration wird persistent gespeichert; Serveraktivierung erfolgt über das Konto"
            })
            .active(false)
            .build();
        group_status.add(&row_enabled);

        let row_schedule = adw::SwitchRow::builder()
            .title("Nur in folgendem Zeitraum senden")
            .active(true)
            .build();
        group_status.add(&row_schedule);

        let row_start = adw::EntryRow::builder()
            .title("Startdatum (YYYY-MM-DD, optional)")
            .build();
        group_status.add(&row_start);

        let row_end = adw::EntryRow::builder()
            .title("Enddatum (YYYY-MM-DD, optional)")
            .build();
        group_status.add(&row_end);

        let saved_settings = OutOfOfficeStore::default().load();
        row_enabled.set_active(saved_settings.enabled);
        row_schedule.set_active(saved_settings.use_schedule);
        row_start.set_text(&saved_settings.start_date);
        row_end.set_text(&saved_settings.end_date);

        content_box.append(&group_status);

        // Internal Message
        let group_internal = adw::PreferencesGroup::builder()
            .title("Nachricht für Kollegen (Intern)")
            .build();

        let text_internal = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_internal.add(&text_internal);
        content_box.append(&group_internal);

        // External Message
        let group_external = adw::PreferencesGroup::builder()
            .title("Nachricht für externe Absender")
            .build();

        let text_external = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_external.add(&text_external);
        content_box.append(&group_external);

        text_internal
            .buffer()
            .set_text(&saved_settings.internal_message);
        text_external
            .buffer()
            .set_text(&saved_settings.external_message);
        row_start.set_sensitive(saved_settings.use_schedule);
        row_end.set_sensitive(saved_settings.use_schedule);
        let row_start_for_schedule = row_start.clone();
        let row_end_for_schedule = row_end.clone();
        row_schedule.connect_active_notify(move |row| {
            row_start_for_schedule.set_sensitive(row.is_active());
            row_end_for_schedule.set_sensitive(row.is_active());
        });

        if let Some(service) = graph_service.clone() {
            load_graph_settings(
                self,
                service,
                &row_enabled,
                &row_schedule,
                &row_start,
                &row_end,
                &text_internal.buffer(),
                &text_external.buffer(),
                &backend_state,
            );
        }

        let dialog_for_save = self.clone();
        let row_enabled_for_save = row_enabled.clone();
        let row_schedule_for_save = row_schedule.clone();
        let row_start_for_save = row_start.clone();
        let row_end_for_save = row_end.clone();
        let internal_buffer_for_save = text_internal.buffer();
        let external_buffer_for_save = text_external.buffer();
        let graph_service_for_save = graph_service.clone();
        let save_button_for_save = btn_save.clone();
        let backend_state_for_save = backend_state.clone();
        btn_save.connect_clicked(move |_| {
            let internal_message = internal_buffer_for_save.text(
                &internal_buffer_for_save.start_iter(),
                &internal_buffer_for_save.end_iter(),
                false,
            );
            let external_message = external_buffer_for_save.text(
                &external_buffer_for_save.start_iter(),
                &external_buffer_for_save.end_iter(),
                false,
            );
            let settings = OutOfOfficeSettings {
                enabled: row_enabled_for_save.is_active(),
                use_schedule: row_schedule_for_save.is_active(),
                start_date: row_start_for_save.text().trim().to_string(),
                end_date: row_end_for_save.text().trim().to_string(),
                internal_message: internal_message.to_string(),
                external_message: external_message.to_string(),
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
                            "Abwesenheitsnotiz lokal gespeichert; die Serveraktivierung erfolgt über das Konto",
                        );
                        dialog_for_save.close();
                        return;
                    };

                    save_button_for_save.set_sensitive(false);
                    backend_state_for_save.set_label("Microsoft Graph: Serverstatus wird gespeichert …");
                    let settings_for_server = settings.clone();
                    let dialog_weak = dialog_for_save.downgrade();
                    let save_button = save_button_for_save.clone();
                    let backend_state = backend_state_for_save.clone();
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
                                    "Abwesenheitsnotiz lokal und auf dem Exchange-Server gespeichert",
                                );
                                dialog.close();
                            }
                            Ok(Err(error)) => {
                                backend_state.set_label("Microsoft Graph: Server konnte nicht aktualisiert werden");
                                show_dialog_toast(
                                    &dialog,
                                    &format!(
                                        "Lokal gespeichert, Serveraktualisierung fehlgeschlagen: {error}"
                                    ),
                                );
                            }
                            Err(error) => {
                                backend_state.set_label("Microsoft Graph: Server konnte nicht aktualisiert werden");
                                show_dialog_toast(
                                    &dialog,
                                    &format!("Serveraktualisierung abgebrochen: {error}"),
                                );
                            }
                        }
                    });
                }
                Err(error) => show_dialog_toast(
                    &dialog_for_save,
                    &format!("Abwesenheitsnotiz konnte nicht gespeichert werden: {error}"),
                ),
            }
        });

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
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
    row_schedule: &adw::SwitchRow,
    row_start: &adw::EntryRow,
    row_end: &adw::EntryRow,
    internal_buffer: &gtk::TextBuffer,
    external_buffer: &gtk::TextBuffer,
    backend_state: &gtk::Label,
) {
    let dialog_weak = dialog.downgrade();
    let row_enabled = row_enabled.clone();
    let row_schedule = row_schedule.clone();
    let row_start = row_start.clone();
    let row_end = row_end.clone();
    let internal_buffer = internal_buffer.clone();
    let external_buffer = external_buffer.clone();
    let backend_state = backend_state.clone();
    let (sender, receiver) = async_channel::bounded(1);
    thread::spawn(move || {
        let result = service.get_out_of_office_sync();
        let _ = sender.send_blocking(result);
    });
    glib::spawn_future_local(async move {
        let result = receiver.recv().await;
        let Some(_dialog) = dialog_weak.upgrade() else {
            return;
        };
        match result {
            Ok(Ok(settings)) => {
                row_enabled.set_active(settings.enabled);
                row_schedule.set_active(settings.use_schedule);
                row_start.set_text(&settings.start_date);
                row_end.set_text(&settings.end_date);
                internal_buffer.set_text(&settings.internal_message);
                external_buffer.set_text(&settings.external_message);
                backend_state.set_label("Microsoft Graph: Serverstatus geladen");
            }
            Ok(Err(error)) => {
                backend_state.set_label(
                    "Microsoft Graph: Serverstatus nicht verfügbar; lokale Werte bleiben erhalten",
                );
                tracing::warn!(%error, "Could not load Graph out-of-office settings");
            }
            Err(error) => {
                backend_state.set_label(
                    "Microsoft Graph: Serverstatus nicht verfügbar; lokale Werte bleiben erhalten",
                );
                tracing::warn!(%error, "Graph out-of-office load was cancelled");
            }
        }
    });
}

fn show_dialog_toast(dialog: &OutOfOfficeDialog, message: &str) {
    let toast = adw::Toast::new(message);
    if let Some(window) = dialog.root().and_downcast::<crate::window::EvolutionWindow>() {
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
