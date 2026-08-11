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

use crate::services::out_of_office_service::{OutOfOfficeSettings, OutOfOfficeStore};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct OutOfOfficeDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for OutOfOfficeDialog {
        const NAME: &'static str = "EvolutionOutOfOfficeDialog";
        type Type = super::OutOfOfficeDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for OutOfOfficeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for OutOfOfficeDialog {}
    impl WindowImpl for OutOfOfficeDialog {}
    impl AdwWindowImpl for OutOfOfficeDialog {}
}

glib::wrapper! {
    pub struct OutOfOfficeDialog(ObjectSubclass<imp::OutOfOfficeDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl OutOfOfficeDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Abwesenheitsnotiz (Auto-Responder)")
            .property("modal", true)
            .property("default-width", 600)
            .property("default-height", 520)
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

        // Status Group
        let group_status = adw::PreferencesGroup::builder()
            .title("Status & Zeitfenster")
            .build();

        let row_enabled = adw::SwitchRow::builder()
            .title("Abwesenheitsnotiz aktivieren")
            .subtitle("Konfiguration wird persistent gespeichert; Serveraktivierung folgt über EDS")
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

        let dialog_for_save = self.clone();
        let row_enabled_for_save = row_enabled.clone();
        let row_schedule_for_save = row_schedule.clone();
        let row_start_for_save = row_start.clone();
        let row_end_for_save = row_end.clone();
        let internal_buffer_for_save = text_internal.buffer();
        let external_buffer_for_save = text_external.buffer();
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
                    show_dialog_toast(
                        &dialog_for_save,
                        "Abwesenheitsnotiz lokal gespeichert; EDS-Serveraktivierung bleibt ausstehend",
                    );
                    dialog_for_save.close();
                }
                Err(error) => show_dialog_toast(
                    &dialog_for_save,
                    &format!("Abwesenheitsnotiz konnte nicht gespeichert werden: {error}"),
                ),
            }
        });

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

fn show_dialog_toast(dialog: &OutOfOfficeDialog, message: &str) {
    let toast = adw::Toast::new(message);
    if let Some(parent) = dialog.transient_for() {
        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&toast);
            return;
        }
    }
    tracing::info!(%message, "Out-of-office dialog result");
}

impl Default for OutOfOfficeDialog {
    fn default() -> Self {
        Self::new()
    }
}
