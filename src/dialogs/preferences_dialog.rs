// SPDX-License-Identifier: GPL-3.0-or-later
//
// PreferencesDialog — Application settings and account configuration.
//
// Uses AdwPreferencesWindow with groups for:
// - E-Mail-Konten (EDS Integration, Accounts)
// - Sicherheit & PGP/S-MIME
// - Erscheinungsbild & Leseansicht (Dark Mode, Remote Images)
// - Benachrichtigungen

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use crate::services::AppSettings;
use crate::services::account_config_service::AccountConfigStore;
use crate::services::backup_service;
use crate::services::calendar_category_service::{CalendarCategory, CalendarCategoryStore};
use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::eds_service::{self, EdsSourceKind};
use crate::services::eds_source_cache_service::EdsSourceCache;
use crate::services::mail_filter_service::{
    FilterAction, FilterField, FilterOperator, MailFilterStore,
};
use crate::services::mail_store_service::MailStoreService;
use crate::services::migration_service;
use crate::services::signature_store_service::SignatureStore;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct PreferencesDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "EvolutionPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;
    }

    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreferencesDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        let settings = AppSettings::try_new();

        // ── 1. Page: Konten & Mail ──────────────────────────────────────────
        let page_accounts = adw::PreferencesPage::builder()
            .title("Konten")
            .icon_name("avatar-default-symbolic")
            .build();

        let group_accounts = adw::PreferencesGroup::builder()
            .title("E-Mail-Konten (EDS)")
            .description("Verwaltete Konten im Evolution Data Server")
            .build();

        let no_accounts = adw::ActionRow::builder()
            .title("Keine EDS-Konten gefunden")
            .subtitle("Konten werden nach der EDS-Integration hier verwaltet")
            .build();
        let icon_no_accounts = gtk::Image::from_icon_name("dialog-information-symbolic");
        no_accounts.add_prefix(&icon_no_accounts);
        let row_add_account = adw::ActionRow::builder()
            .title("Neues Konto hinzufügen…")
            .subtitle("IMAP, POP3, Exchange (EWS) oder GOA")
            .activatable(true)
            .build();
        let icon_add = gtk::Image::from_icon_name("list-add-symbolic");
        row_add_account.add_prefix(&icon_add);

        row_add_account.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::AccountSetupDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));
        let row_oof = adw::ActionRow::builder()
            .title("Abwesenheitsnotiz (Auto-Responder)…")
            .subtitle("Automatische Antwort für Urlaub und Abwesenheiten einrichten")
            .activatable(true)
            .build();
        let icon_oof = gtk::Image::from_icon_name("mail-outbox-symbolic");
        row_oof.add_prefix(&icon_oof);

        row_oof.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::OutOfOfficeDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));
        group_accounts.add(&row_oof);

        group_accounts.add(&no_accounts);
        group_accounts.add(&row_add_account);

        page_accounts.add(&group_accounts);

        let group_profiles = adw::PreferencesGroup::builder()
            .title("Lokale Kontoprofile")
            .description(
                "Nicht-geheime Profile für die spätere EDS/Camel-Aktivierung; Passwörter werden nicht gespeichert",
            )
            .build();
        let profiles = AccountConfigStore::default().load();
        if profiles.is_empty() {
            group_profiles.add(
                &adw::ActionRow::builder()
                    .title("Keine lokalen Kontoprofile")
                    .subtitle("Über „Neues Konto hinzufügen…“ ein Profil anlegen")
                    .build(),
            );
        } else {
            for profile in profiles {
                let subtitle = format!("{} · {}", profile.email, profile.protocol.as_str());
                let row = adw::ActionRow::builder()
                    .title(&profile.display_name)
                    .subtitle(&subtitle)
                    .activatable(true)
                    .build();
                let profile_for_edit = profile.clone();
                row.connect_activated(glib::clone!(
                    #[weak(rename_to = page)]
                    self,
                    move |_| {
                        let dialog =
                            crate::dialogs::AccountSetupDialog::new_for_edit(&profile_for_edit);
                        let root = page.root();
                        if let Some(window) =
                            root.and_then(|root| root.downcast::<gtk::Window>().ok())
                        {
                            dialog.set_transient_for(Some(&window));
                        }
                        dialog.present();
                    }
                ));
                let enabled = gtk::Switch::builder()
                    .active(profile.enabled)
                    .valign(gtk::Align::Center)
                    .tooltip_text("Profil aktivieren/deaktivieren")
                    .build();
                let profile_id = profile.id.clone();
                enabled.connect_active_notify(move |switch| {
                    if let Err(error) =
                        AccountConfigStore::default().set_enabled(&profile_id, switch.is_active())
                    {
                        tracing::warn!(%error, "Could not update local account profile state");
                    }
                });
                row.add_suffix(&enabled);

                let default_button = gtk::Button::builder()
                    .icon_name("starred-symbolic")
                    .tooltip_text("Als Standardkonto für neue Nachrichten verwenden")
                    .css_classes(vec!["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                let profile_id_for_default = profile.id.clone();
                let settings_for_default = settings.clone();
                let dialog_weak_for_default = self.downgrade();
                default_button.connect_clicked(move |_| {
                    let Some(settings) = settings_for_default.as_ref() else {
                        return;
                    };
                    settings.set_default_account_id(&profile_id_for_default);
                    if let Some(dialog) = dialog_weak_for_default.upgrade() {
                        dialog.show_toast("Standardkonto gespeichert");
                    }
                });
                row.add_suffix(&default_button);

                let delete_button = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text("Lokales Profil entfernen")
                    .css_classes(vec!["flat", "error"])
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&delete_button);
                let dialog_weak_for_delete = self.downgrade();
                let row_for_delete = row.clone();
                let profile_id_for_delete = profile.id.clone();
                delete_button.connect_clicked(move |_| {
                    let Some(dialog) = dialog_weak_for_delete.upgrade() else {
                        return;
                    };
                    let row = row_for_delete.clone();
                    let profile_id = profile_id_for_delete.clone();
                    let dialog_for_action = dialog.clone();
                    crate::dialogs::run_destructive_action(
                        &dialog,
                        "Lokales Kontoprofil entfernen?",
                        "Nur das app-eigene, geheimnisfreie Profil wird entfernt. EDS, Camel und der Secret Service bleiben unverändert.",
                        "Entfernen",
                        move || match AccountConfigStore::default().delete(&profile_id) {
                            Ok(true) => {
                                row.set_visible(false);
                                dialog_for_action.show_toast("Lokales Kontoprofil entfernt");
                            }
                            Ok(false) => {
                                dialog_for_action
                                    .show_toast("Lokales Kontoprofil wurde nicht gefunden")
                            }
                            Err(error) => dialog_for_action.show_toast(&format!(
                                "Kontoprofil konnte nicht entfernt werden: {error}"
                            )),
                        },
                    );
                });
                group_profiles.add(&row);
            }
        }
        page_accounts.add(&group_profiles);

        // Filter Rules Section
        let group_filters = adw::PreferencesGroup::builder()
            .title("Mail-Filter & Sortierung")
            .description(
                "Automatische Regeln für eingehende Mails und manuelle Ausführung im lokalen Bestand",
            )
            .build();

        let row_add_filter = adw::ActionRow::builder()
            .title("Neue Filterregel erstellen…")
            .subtitle("Bedingungen festlegen (Betreff, Absender, Zielordner)")
            .activatable(true)
            .build();
        let icon_filter = gtk::Image::from_icon_name("funnel-symbolic");
        row_add_filter.add_prefix(&icon_filter);

        row_add_filter.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::FilterEditorDialog::new();
                let root = page.root();
                if let Some(win) = root.and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.set_transient_for(Some(&win));
                }
                dialog.present();
            }
        ));
        group_filters.add(&row_add_filter);
        for rule in MailFilterStore::default().load() {
            let condition_text = rule
                .effective_conditions()
                .into_iter()
                .map(|condition| {
                    let field = filter_field_label(condition.field);
                    let operator = filter_operator_label(condition.operator);
                    if condition.value.is_empty() {
                        format!("{field} {operator}")
                    } else {
                        format!("{field} {operator} „{}“", condition.value)
                    }
                })
                .collect::<Vec<_>>()
                .join(if rule.match_all { " UND " } else { " ODER " });
            let action_text = rule
                .effective_actions()
                .into_iter()
                .map(filter_action_label)
                .collect::<Vec<_>>()
                .join(", ");
            let subtitle = format!("{condition_text} · {action_text}");
            let row = adw::ActionRow::builder()
                .title(&rule.name)
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            let rule_for_edit = rule.clone();
            row.connect_activated(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    let dialog = crate::dialogs::FilterEditorDialog::new_for_edit(&rule_for_edit);
                    let root = page.root();
                    if let Some(window) = root.and_then(|root| root.downcast::<gtk::Window>().ok())
                    {
                        dialog.set_transient_for(Some(&window));
                    }
                    dialog.present();
                }
            ));
            let enabled = gtk::Switch::builder()
                .active(rule.enabled)
                .valign(gtk::Align::Center)
                .tooltip_text("Filterregel aktivieren/deaktivieren")
                .build();
            let rule_id = rule.id.clone();
            enabled.connect_active_notify(move |switch| {
                if let Err(error) =
                    MailFilterStore::default().set_enabled(&rule_id, switch.is_active())
                {
                    tracing::warn!(%error, "Could not update mail filter state");
                }
            });
            row.add_suffix(&enabled);

            let move_up_button = gtk::Button::builder()
                .icon_name("go-up-symbolic")
                .tooltip_text("Filterregel nach oben verschieben")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let move_down_button = gtk::Button::builder()
                .icon_name("go-down-symbolic")
                .tooltip_text("Filterregel nach unten verschieben")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let rule_id_for_up = rule.id.clone();
            let dialog_weak_for_up = self.downgrade();
            move_up_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_up.upgrade() else {
                    return;
                };
                match MailFilterStore::default().move_rule(&rule_id_for_up, -1) {
                    Ok(true) => dialog.show_toast("Filterreihenfolge gespeichert"),
                    Ok(false) => dialog.show_toast("Filterregel steht bereits an erster Stelle"),
                    Err(error) => dialog.show_toast(&format!(
                        "Filterreihenfolge konnte nicht gespeichert werden: {error}"
                    )),
                }
            });
            let rule_id_for_down = rule.id.clone();
            let dialog_weak_for_down = self.downgrade();
            move_down_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_down.upgrade() else {
                    return;
                };
                match MailFilterStore::default().move_rule(&rule_id_for_down, 1) {
                    Ok(true) => dialog.show_toast("Filterreihenfolge gespeichert"),
                    Ok(false) => dialog.show_toast("Filterregel steht bereits an letzter Stelle"),
                    Err(error) => dialog.show_toast(&format!(
                        "Filterreihenfolge konnte nicht gespeichert werden: {error}"
                    )),
                }
            });
            row.add_suffix(&move_up_button);
            row.add_suffix(&move_down_button);

            let run_button = gtk::Button::builder()
                .icon_name("media-playback-start-symbolic")
                .tooltip_text("Regel jetzt im lokalen Mailbestand ausführen")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let rule_id_for_run = rule.id.clone();
            let dialog_weak_for_run = self.downgrade();
            let run_button_for_result = run_button.clone();
            run_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_run.upgrade() else {
                    return;
                };
                run_button_for_result.set_sensitive(false);
                let (sender, receiver) = async_channel::bounded(1);
                let rule_id = rule_id_for_run.clone();
                std::thread::spawn(move || {
                    let result = MailStoreService::new().run_filter_rule(&rule_id);
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Manual filter result was dropped");
                    }
                });
                let dialog_weak = dialog.downgrade();
                let run_button = run_button_for_result.clone();
                glib::spawn_future_local(async move {
                    let result = receiver.recv().await;
                    run_button.set_sensitive(true);
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(Ok(Some(report))) => {
                            if let Some(window) = dialog
                                .root()
                                .and_downcast::<crate::window::EvolutionWindow>()
                            {
                                window.refresh_mail_page();
                            }
                            dialog.show_toast(&format!(
                                "{} Treffer · {} geändert · {} verschoben · {} kopiert · {} gelöscht",
                                report.matched_messages,
                                report.changed_messages,
                                report.moved_messages,
                                report.copied_messages,
                                report.deleted_messages,
                            ));
                        }
                        Ok(Ok(None)) => dialog.show_toast("Filterregel wurde nicht gefunden"),
                        Ok(Err(error)) => dialog
                            .show_toast(&format!("Lokaler Filterlauf fehlgeschlagen: {error}")),
                        Err(error) => dialog
                            .show_toast(&format!("Lokaler Filterlauf abgebrochen: {error}")),
                    }
                });
            });
            row.add_suffix(&run_button);

            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Filterregel entfernen")
                .css_classes(vec!["flat", "error"])
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&delete_button);
            let dialog_weak_for_delete = self.downgrade();
            let row_for_delete = row.clone();
            let rule_id_for_delete = rule.id.clone();
            delete_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_delete.upgrade() else {
                    return;
                };
                let row = row_for_delete.clone();
                let rule_id = rule_id_for_delete.clone();
                let dialog_for_action = dialog.clone();
                crate::dialogs::run_destructive_action(
                    &dialog,
                    "Filterregel entfernen?",
                    "Die Regel wird aus dem app-eigenen Filterbestand entfernt.",
                    "Entfernen",
                    move || match MailFilterStore::default().delete(&rule_id) {
                        Ok(true) => {
                            row.set_visible(false);
                            dialog_for_action.show_toast("Filterregel entfernt");
                        }
                        Ok(false) => {
                            dialog_for_action.show_toast("Filterregel wurde nicht gefunden")
                        }
                        Err(error) => dialog_for_action.show_toast(&format!(
                            "Filterregel konnte nicht entfernt werden: {error}"
                        )),
                    },
                );
            });
            group_filters.add(&row);
        }
        page_accounts.add(&group_filters);

        let group_sync = adw::PreferencesGroup::builder()
            .title("Synchronisation")
            .description("Automatische Aktualisierung der EDS/Camel-Mailordner")
            .build();
        let row_sync_interval = adw::SpinRow::with_range(0.0, 86_400.0, 5.0);
        row_sync_interval.set_title("Mail-Aktualisierungsintervall");
        row_sync_interval.set_subtitle("Sekunden; 0 deaktiviert die automatische Aktualisierung");
        row_sync_interval.set_digits(0);
        row_sync_interval.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_sync_interval.set_value(settings.mail_sync_interval_seconds() as f64);
            row_sync_interval.connect_value_notify(move |row| {
                settings.set_mail_sync_interval_seconds(row.value().round() as u32);
            });
        }
        group_sync.add(&row_sync_interval);

        let row_draft_autosave = adw::SpinRow::with_range(0.0, 3_600.0, 5.0);
        row_draft_autosave.set_title("Entwürfe automatisch speichern");
        row_draft_autosave.set_subtitle(
            "Sekunden; 0 deaktiviert das lokale Autosave (auf Linux übernimmt später EDS/Camel)",
        );
        row_draft_autosave.set_digits(0);
        row_draft_autosave.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_draft_autosave.set_value(settings.draft_autosave_interval_seconds() as f64);
            row_draft_autosave.connect_value_notify(move |row| {
                settings.set_draft_autosave_interval_seconds(row.value().round() as u32);
            });
        }
        group_sync.add(&row_draft_autosave);

        let row_local_mail = directory_picker_row(
            self,
            "Lokaler Mailordner",
            "Automatisch erkannter Evolution-Ordner",
            settings
                .as_ref()
                .map(AppSettings::local_mail_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_mail_directory(path),
        );
        group_sync.add(&row_local_mail);
        page_accounts.add(&group_sync);

        let group_local_sources = adw::PreferencesGroup::builder()
            .title("Lokale Importquellen")
            .description(
                "Externe EML/Maildir/mbox-, iCalendar- und vCard-Dateien bleiben \
                 schreibgeschützt; app-eigene lokale Quellen können bearbeitet werden",
            )
            .build();
        let row_local_calendar = directory_picker_row(
            self,
            "Lokaler Kalenderordner",
            "Automatisch erkannter iCalendar-Ordner",
            settings
                .as_ref()
                .map(AppSettings::local_calendar_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_calendar_directory(path),
        );
        group_local_sources.add(&row_local_calendar);
        let row_local_contacts = directory_picker_row(
            self,
            "Lokaler Kontaktordner",
            "Automatisch erkannter vCard-Ordner",
            settings
                .as_ref()
                .map(AppSettings::local_contact_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_contact_directory(path),
        );
        group_local_sources.add(&row_local_contacts);

        let row_import_calendar = adw::ActionRow::builder()
            .title("iCalendar-Dateien importieren")
            .subtitle("ICS/VCS-Dateien nicht überschreibend in den lokalen Kalender kopieren")
            .build();
        let import_calendar_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("iCalendar-Dateien auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_calendar.add_suffix(&import_calendar_button);
        import_calendar_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für den Kalenderimport verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("iCalendar-Dateien auswählen")
                    .modal(true)
                    .build();
                let dialog_for_file = dialog.downgrade();
                file_dialog.open_multiple(
                    Some(&root),
                    None::<&gio::Cancellable>,
                    move |result| {
                        let Ok(model) = result else {
                            return;
                        };
                        let paths = selected_file_paths(&model);
                        if paths.is_empty() {
                            return;
                        }
                        let destination = crate::services::CalendarStoreService::new()
                            .calendar_dir()
                            .to_path_buf();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = crate::services::CalendarStoreService::with_calendar_dir(
                                destination,
                            )
                            .import_files(&paths);
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Calendar import result was dropped");
                            }
                        });
                        glib::spawn_future_local(async move {
                            let Ok(result) = receiver.recv().await else {
                                return;
                            };
                            if let Some(dialog) = dialog_for_file.upgrade() {
                                match result {
                                    Ok(count) => dialog.show_toast(&format!(
                                        "{} iCalendar-Datei(en) importiert; Kalenderansicht aktualisieren",
                                        count
                                    )),
                                    Err(error) => dialog.show_toast(&format!(
                                        "Kalenderimport fehlgeschlagen: {error}"
                                    )),
                                }
                            }
                        });
                    },
                );
            }
        ));
        group_local_sources.add(&row_import_calendar);

        let row_import_contacts = adw::ActionRow::builder()
            .title("vCard-Dateien importieren")
            .subtitle("VCF/vCard-Dateien nicht überschreibend in das lokale Adressbuch kopieren")
            .build();
        let import_contacts_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("vCard-Dateien auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_contacts.add_suffix(&import_contacts_button);
        import_contacts_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für den Kontaktimport verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("vCard-Dateien auswählen")
                    .modal(true)
                    .build();
                let dialog_for_file = dialog.downgrade();
                file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(model) = result else {
                        return;
                    };
                    let paths = selected_file_paths(&model);
                    if paths.is_empty() {
                        return;
                    }
                    let destination = crate::services::ContactStoreService::new()
                        .contact_dir()
                        .to_path_buf();
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result =
                            crate::services::ContactStoreService::with_contact_dir(destination)
                                .import_files(&paths);
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Contact import result was dropped");
                        }
                    });
                    glib::spawn_future_local(async move {
                        let Ok(result) = receiver.recv().await else {
                            return;
                        };
                        if let Some(dialog) = dialog_for_file.upgrade() {
                            match result {
                                Ok(count) => dialog.show_toast(&format!(
                                    "{} vCard-Datei(en) importiert; Kontaktansicht aktualisieren",
                                    count
                                )),
                                Err(error) => dialog
                                    .show_toast(&format!("Kontaktimport fehlgeschlagen: {error}")),
                            }
                        }
                    });
                });
            }
        ));
        group_local_sources.add(&row_import_contacts);
        page_accounts.add(&group_local_sources);

        let group_default_address_book = adw::PreferencesGroup::builder()
            .title("Standardadressbuch")
            .description("Neue Kontakte und die lokale Kontaktsuche verwenden diese Quelle")
            .build();
        group_default_address_book.add(&source_preference_row(
            "Standardadressbuch",
            "EDS-Adressbücher und lokales Adressbuch",
            EdsSourceKind::AddressBook,
            "local-addressbook",
            "Lokales Adressbuch",
            settings.clone(),
            AppSettings::default_address_book_source_uid,
            AppSettings::set_default_address_book_source_uid,
        ));
        page_accounts.add(&group_default_address_book);

        let group_backup = adw::PreferencesGroup::builder()
            .title("Daten &amp; Sicherung")
            .description(
                "App-eigene Zustände sichern, diagnostizieren, migrieren oder löschen; das originale EDS-Profil bleibt unberührt",
            )
            .build();
        let row_backup = adw::ActionRow::builder()
            .title("App-Daten sichern")
            .subtitle("Entwürfe, Filter, lokale Zustände und Kontoprofile ohne Geheimnisse")
            .build();
        let backup_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Sicherung erstellen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_backup.add_suffix(&backup_button);
        backup_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let raw = match backup_service::create_backup() {
                    Ok(raw) => raw,
                    Err(error) => {
                        dialog.show_toast(&format!(
                            "Sicherung konnte nicht erstellt werden: {error}"
                        ));
                        return;
                    }
                };
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für die Sicherung verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Evolution GTK4-Sicherung speichern")
                    .initial_name("evolution-gtk4-backup.json")
                    .modal(true)
                    .build();
                let dialog_for_result = dialog.downgrade();
                file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let save_result = file.replace_contents(
                        &raw,
                        None,
                        true,
                        gio::FileCreateFlags::NONE,
                        None::<&gio::Cancellable>,
                    );
                    if let Some(dialog) = dialog_for_result.upgrade() {
                        match save_result {
                            Ok(_) => dialog.show_toast("App-Daten gesichert"),
                            Err(error) => dialog.show_toast(&format!(
                                "Sicherung konnte nicht gespeichert werden: {error}"
                            )),
                        }
                    }
                });
            }
        ));
        group_backup.add(&row_backup);

        let row_restore = adw::ActionRow::builder()
            .title("App-Daten wiederherstellen")
            .subtitle("Ersetzt nur die in der Sicherung enthaltenen app-eigenen Dateien")
            .build();
        let restore_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Sicherung wiederherstellen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_restore.add_suffix(&restore_button);
        restore_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für die Wiederherstellung verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Evolution GTK4-Sicherung öffnen")
                    .modal(true)
                    .build();
                let dialog_for_file = dialog.downgrade();
                file_dialog.open(
                    Some(&root),
                    None::<&gio::Cancellable>,
                    move |result| {
                        let Ok(file) = result else {
                            return;
                        };
                        let Ok((contents, _etag)) = file.load_contents(None::<&gio::Cancellable>)
                        else {
                            if let Some(dialog) = dialog_for_file.upgrade() {
                                dialog.show_toast("Sicherung konnte nicht gelesen werden");
                            }
                            return;
                        };
                        let bytes = contents.as_ref().to_vec();
                        let Some(dialog) = dialog_for_file.upgrade() else {
                            return;
                        };
                        let confirmation = adw::AlertDialog::new(
                            Some("App-Daten wiederherstellen?"),
                            Some("Vorhandene app-eigene Dateien aus dieser Sicherung werden ersetzt. Das originale EDS-Profil bleibt unverändert."),
                        );
                        confirmation.add_responses(&[
                            ("cancel", "Abbrechen"),
                            ("restore", "Wiederherstellen"),
                        ]);
                        confirmation.set_response_appearance(
                            "restore",
                            adw::ResponseAppearance::Destructive,
                        );
                        confirmation.connect_response(
                            None,
                            glib::clone!(
                                #[weak]
                                dialog,
                                move |_confirmation, response| {
                                    if response != "restore" {
                                        return;
                                    }
                                    match backup_service::restore_backup(&bytes) {
                                        Ok(report) => dialog.show_toast(&format!(
                                            "App-Daten wiederhergestellt ({} Dateien{})",
                                            report.files_restored,
                                            if report.settings_restored {
                                                ", Einstellungen übernommen"
                                            } else {
                                                ""
                                            }
                                        )),
                                        Err(error) => dialog.show_toast(&format!(
                                            "Wiederherstellung fehlgeschlagen: {error}"
                                        )),
                                    }
                                }
                            ),
                        );
                        confirmation.present(Some(&dialog));
                    },
                );
            }
        ));
        group_backup.add(&row_restore);

        let row_diagnostic = adw::ActionRow::builder()
            .title("Diagnosepaket erstellen")
            .subtitle(
                "Nur technische Metadaten und Dateigrößen; keine Mailinhalte oder Geheimnisse",
            )
            .build();
        let diagnostic_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Diagnosepaket speichern")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_diagnostic.add_suffix(&diagnostic_button);
        diagnostic_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let raw = match backup_service::create_diagnostic_package() {
                    Ok(raw) => raw,
                    Err(error) => {
                        dialog.show_toast(&format!(
                            "Diagnosepaket konnte nicht erstellt werden: {error}"
                        ));
                        return;
                    }
                };
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für das Diagnosepaket verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Diagnosepaket speichern")
                    .initial_name("evolution-gtk4-diagnostic.json")
                    .modal(true)
                    .build();
                let dialog_for_result = dialog.downgrade();
                file_dialog.save(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let save_result = file.replace_contents(
                        &raw,
                        None,
                        true,
                        gio::FileCreateFlags::NONE,
                        None::<&gio::Cancellable>,
                    );
                    if let Some(dialog) = dialog_for_result.upgrade() {
                        match save_result {
                            Ok(_) => dialog.show_toast("Diagnosepaket gespeichert"),
                            Err(error) => dialog.show_toast(&format!(
                                "Diagnosepaket konnte nicht gespeichert werden: {error}"
                            )),
                        }
                    }
                });
            }
        ));
        group_backup.add(&row_diagnostic);

        let row_delete = adw::ActionRow::builder()
            .title("App-eigene Daten löschen")
            .subtitle(
                "Kontenprofile, Entwürfe, Filter, lokale App-Stores und Einstellungen entfernen",
            )
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("App-eigene Daten löschen")
            .css_classes(vec!["flat", "error"])
            .valign(gtk::Align::Center)
            .build();
        row_delete.add_suffix(&delete_button);
        delete_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let confirmation = adw::AlertDialog::new(
                    Some("App-eigene Daten löschen?"),
                    Some("Kontenprofile, Entwürfe, lokale ICS-/vCard-Daten und app-eigene Einstellungen werden entfernt. Das originale EDS-Profil, der Secret Service und ausgewählte externe Ordner bleiben unverändert."),
                );
                confirmation.add_responses(&[
                    ("cancel", "Abbrechen"),
                    ("delete", "App-Daten löschen"),
                ]);
                confirmation.set_response_appearance(
                    "delete",
                    adw::ResponseAppearance::Destructive,
                );
                confirmation.connect_response(
                    None,
                    glib::clone!(
                        #[weak]
                        dialog,
                        move |_confirmation, response| {
                            if response != "delete" {
                                return;
                            }
                            match backup_service::delete_app_data() {
                                Ok(report) => dialog.show_toast(&format!(
                                    "{} app-eigene Datei(en) gelöscht{}",
                                    report.files_removed,
                                    if report.settings_reset {
                                        "; Einstellungen zurückgesetzt"
                                    } else {
                                        ""
                                    }
                                )),
                                Err(error) => dialog.show_toast(&format!(
                                    "App-Daten konnten nicht vollständig gelöscht werden: {error}"
                                )),
                            }
                        }
                    ),
                );
                confirmation.present(Some(&dialog));
            }
        ));
        group_backup.add(&row_delete);

        let row_import_profile = adw::ActionRow::builder()
            .title("Evolution-Profil importieren")
            .subtitle("Nicht-geheime Kontenmetadaten aus einer ausgewählten Profilkopie übernehmen")
            .build();
        let import_profile_button = gtk::Button::builder()
            .icon_name("document-import-symbolic")
            .tooltip_text("Evolution-Profilordner auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_profile.add_suffix(&import_profile_button);
        import_profile_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für den Profilimport verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Evolution-Profilordner auswählen")
                    .modal(true)
                    .build();
                let dialog_for_file = dialog.downgrade();
                file_dialog.select_folder(
                    Some(&root),
                    None::<&gio::Cancellable>,
                    move |result| {
                        let Ok(file) = result else {
                            return;
                        };
                        let Some(path) = file.path() else {
                            return;
                        };
                        let source_cache = EdsSourceCache::default();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = migration_service::import_evolution_sources_with_cache(
                                path,
                                &AccountConfigStore::default(),
                                &source_cache,
                            );
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Profile migration result was dropped");
                            }
                        });
                        glib::spawn_future_local(async move {
                            let Ok(result) = receiver.recv().await else {
                                return;
                            };
                            let Some(dialog) = dialog_for_file.upgrade() else {
                                return;
                            };
                            match result {
                                Ok(report) => {
                                    if let Some(signature) = report.signature_text.as_deref() {
                                        if let Some(settings) = AppSettings::try_new() {
                                            settings.set_signature_text(signature);
                                            settings.set_append_signature(true);
                                        }
                                    }
                                    for warning in &report.warnings {
                                        tracing::warn!(%warning, "Evolution-Profilimport mit Hinweis");
                                    }
                                    let warning_suffix = if report.warnings.is_empty() {
                                        String::new()
                                    } else {
                                        format!(", {} Hinweis(e) protokolliert", report.warnings.len())
                                    };
                                    let secret_suffix = if report.ignored_secret_fields == 0 {
                                        String::new()
                                    } else {
                                        format!(
                                            "; {} geheime(s) Feld(er) bewusst verworfen",
                                            report.ignored_secret_fields
                                        )
                                    };
                                    dialog.show_toast(&format!(
                                        "{} Profilquelle(n) gefunden: {} Konto/Konten, {} Mail-, {} Kalender- und {} Adressbuchquelle(n) zwischengespeichert, {} Filterregel(n), {} Signatur(en) übernommen, {} übersprungen{}{}",
                                        report.discovered_sources,
                                        report.imported_accounts,
                                        report.cached_mail_sources,
                                        report.cached_calendar_sources,
                                        report.cached_address_book_sources,
                                        report.imported_filters,
                                        report.imported_signatures,
                                        report.skipped_sources,
                                        warning_suffix,
                                        secret_suffix
                                    ));
                                }
                                Err(error) => dialog
                                    .show_toast(&format!("Profilimport fehlgeschlagen: {error}")),
                            }
                        });
                    },
                );
            }
        ));
        group_backup.add(&row_import_profile);

        let row_import_mail = adw::ActionRow::builder()
            .title("Lokale Maildateien importieren")
            .subtitle("EML-, mbox- oder Maildir-Dateien/-Ordner in den konfigurierten lokalen Importordner kopieren")
            .build();
        let import_mail_button = gtk::Button::builder()
            .icon_name("mail-folder-inbox-symbolic")
            .tooltip_text("Maildateien auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_mail.add_suffix(&import_mail_button);
        import_mail_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für den Mailimport verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Lokale Maildateien auswählen")
                    .modal(true)
                    .build();
                let dialog_for_file = dialog.downgrade();
                file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                    let Ok(model) = result else {
                        return;
                    };
                    let mut paths = Vec::new();
                    for index in 0..model.n_items() {
                        if let Some(path) = model
                            .item(index)
                            .and_downcast::<gio::File>()
                            .and_then(|file| file.path())
                        {
                            paths.push(path);
                        }
                    }
                    if paths.is_empty() {
                        return;
                    }
                    let destination = crate::services::MailStoreService::new().mail_dir.clone();
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = crate::services::MailStoreService::with_mail_dir(destination)
                            .import_files(&paths);
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Mail import result was dropped");
                        }
                    });
                    glib::spawn_future_local(async move {
                        let Ok(result) = receiver.recv().await else {
                            return;
                        };
                        if let Some(dialog) = dialog_for_file.upgrade() {
                            match result {
                                Ok(count) => dialog.show_toast(&format!(
                                    "{} lokale Maildatei(en) importiert; Mailansicht aktualisieren",
                                    count
                                )),
                                Err(error) => dialog
                                    .show_toast(&format!("Mailimport fehlgeschlagen: {error}")),
                            }
                        }
                    });
                });
            }
        ));
        let import_mail_folder_button = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Mailordner auswählen")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_mail.add_suffix(&import_mail_folder_button);
        import_mail_folder_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("Kein Fenster für den Mailordnerimport verfügbar");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Lokalen Mailordner auswählen")
                    .modal(true)
                    .build();
                let dialog_for_folder = dialog.downgrade();
                file_dialog.select_folder(
                    Some(&root),
                    None::<&gio::Cancellable>,
                    move |result| {
                        let Ok(folder) = result else {
                            return;
                        };
                        let Some(path) = folder.path() else {
                            return;
                        };
                        let destination =
                            crate::services::MailStoreService::new().mail_dir.clone();
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = crate::services::MailStoreService::with_mail_dir(destination)
                                .import_files(std::slice::from_ref(&path));
                            if let Err(error) = sender.send_blocking(result) {
                                tracing::debug!(%error, "Mail folder import result was dropped");
                            }
                        });
                        glib::spawn_future_local(async move {
                            let Ok(result) = receiver.recv().await else {
                                return;
                            };
                            if let Some(dialog) = dialog_for_folder.upgrade() {
                                match result {
                                    Ok(count) => dialog.show_toast(&format!(
                                        "{} Maildatei(en) aus dem Ordner importiert; Mailansicht aktualisieren",
                                        count
                                    )),
                                    Err(error) => dialog.show_toast(&format!(
                                        "Mailordnerimport fehlgeschlagen: {error}"
                                    )),
                                }
                            }
                        });
                    },
                );
            }
        ));
        group_backup.add(&row_import_mail);
        page_accounts.add(&group_backup);

        let group_composer = adw::PreferencesGroup::builder()
            .title("Composer")
            .description("Format, Signatur und Vorlagen für neue Nachrichten")
            .build();
        let row_composer_html = adw::SwitchRow::builder()
            .title("HTML beim Verfassen verwenden")
            .subtitle("Deaktivieren, um neue Nachrichten standardmäßig als Nur-Text zu schreiben")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_composer_html.set_active(settings.composer_html());
            row_composer_html.connect_active_notify(move |row| {
                settings.set_composer_html(row.is_active());
            });
        }
        group_composer.add(&row_composer_html);

        let row_spellcheck_languages = adw::ComboRow::builder()
            .title("Rechtschreibprüfung")
            .subtitle(
                "Wörterbuchsprachen für neue Composer (Systemstandard, Deutsch oder Englisch)",
            )
            .model(&gtk::StringList::new(&[
                "Systemstandard",
                "Deutsch",
                "Englisch",
                "Deutsch und Englisch",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_spellcheck_languages.set_selected(match settings.spellcheck_languages().as_str() {
                "de-DE" => 1,
                "en-US" => 2,
                "de-DE,en-US" => 3,
                _ => 0,
            });
            row_spellcheck_languages.connect_selected_notify(move |row| {
                settings.set_spellcheck_languages(match row.selected() {
                    1 => "de-DE",
                    2 => "en-US",
                    3 => "de-DE,en-US",
                    _ => "",
                });
            });
        }
        group_composer.add(&row_spellcheck_languages);

        let row_append_signature = adw::SwitchRow::builder()
            .title("Signatur automatisch einfügen")
            .subtitle("Wird als Klartext am Ende neuer Nachrichten eingefügt")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_append_signature.set_active(settings.append_signature());
            row_append_signature.connect_active_notify(move |row| {
                settings.set_append_signature(row.is_active());
            });
        }
        group_composer.add(&row_append_signature);

        let signature_store = SignatureStore::default();
        let imported_signatures = signature_store.load();
        let signature_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(12)
            .right_margin(12)
            .height_request(90)
            .build();
        let signature_buffer = signature_view.buffer();
        if let Some(settings) = settings.clone() {
            let initial_signature = signature_store
                .active()
                .map(|signature| signature.text)
                .unwrap_or_else(|| settings.signature_text());
            signature_buffer.set_text(&initial_signature);
            let signature_store_for_edit = signature_store.clone();
            signature_buffer.connect_changed(move |buffer| {
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
                settings.set_signature_text(text.as_str());
                let Some(active_id) = signature_store_for_edit.active_id() else {
                    return;
                };
                let Some(mut signature) = signature_store_for_edit
                    .load()
                    .into_iter()
                    .find(|signature| signature.id == active_id)
                else {
                    return;
                };
                signature.text = text.to_string();
                if let Err(error) = signature_store_for_edit.upsert(signature) {
                    tracing::warn!(%error, "Could not update imported signature");
                }
            });
        }

        if !imported_signatures.is_empty() {
            let names = imported_signatures
                .iter()
                .map(|signature| signature.name.as_str())
                .collect::<Vec<_>>();
            let model = gtk::StringList::new(&names);
            let signature_row = adw::ComboRow::builder()
                .title("Aktive Signatur")
                .subtitle("Übernommene Evolution-Signaturen auswählen")
                .model(&model)
                .build();
            if let Some(active_id) = signature_store.active_id() {
                if let Some(index) = imported_signatures
                    .iter()
                    .position(|signature| signature.id == active_id)
                {
                    signature_row.set_selected(index as u32);
                }
            }
            let signature_store_for_selection = signature_store.clone();
            let signature_buffer_for_selection = signature_buffer.clone();
            let settings_for_selection = settings.clone();
            signature_row.connect_selected_notify(move |row| {
                let Some(signature) = imported_signatures.get(row.selected() as usize) else {
                    return;
                };
                if !signature_store_for_selection
                    .set_active(Some(&signature.id))
                    .unwrap_or(false)
                {
                    return;
                }
                signature_buffer_for_selection.set_text(&signature.text);
                if let Some(settings) = settings_for_selection.as_ref() {
                    settings.set_signature_text(&signature.text);
                    settings.set_append_signature(true);
                }
            });
            group_composer.add(&signature_row);
        }

        group_composer.add(&signature_view);
        page_accounts.add(&group_composer);

        let group_reply = adw::PreferencesGroup::builder()
            .title("Antworten &amp; Weiterleiten")
            .description("Verhalten für den übernommenen Originaltext")
            .build();
        let row_quote_original = adw::SwitchRow::builder()
            .title("Originaltext zitieren")
            .subtitle(
                "Fügt den ursprünglichen Nachrichtentext in Antworten und Weiterleitungen ein",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_quote_original.set_active(settings.quote_original());
            row_quote_original.connect_active_notify(move |row| {
                settings.set_quote_original(row.is_active());
            });
        }
        group_reply.add(&row_quote_original);

        let row_quote_style = adw::ComboRow::builder()
            .title("Zitatformat")
            .model(&gtk::StringList::new(&[
                "Blockzitat",
                "Zeilen mit > präfixen",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_quote_style.set_selected(if settings.quote_style() == "prefix" {
                1
            } else {
                0
            });
            row_quote_style.connect_selected_notify(move |row| {
                settings.set_quote_style(if row.selected() == 1 {
                    "prefix"
                } else {
                    "blockquote"
                });
            });
        }
        group_reply.add(&row_quote_style);
        page_accounts.add(&group_reply);

        // ── 2. Page: Erscheinungsbild & Leseansicht ─────────────────────────
        let page_appearance = adw::PreferencesPage::builder()
            .title("Erscheinungsbild")
            .icon_name("display-brightness-symbolic")
            .build();

        let group_reading = adw::PreferencesGroup::builder()
            .title("Leseansicht &amp; Datenschutz")
            .build();

        let row_remote_images = adw::SwitchRow::builder()
            .title("Externe Bilder automatisch laden")
            .subtitle("Aus Sicherheitsgründen wird empfohlen, diese Option deaktiviert zu lassen")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_remote_images.set_active(settings.auto_load_remote_images());
            row_remote_images.connect_active_notify(move |row| {
                settings.set_auto_load_remote_images(row.is_active());
            });
        }
        group_reading.add(&row_remote_images);

        let row_html_rendering = adw::SwitchRow::builder()
            .title("HTML-Nachrichten rendern")
            .subtitle("Deaktivieren für reine Textanzeige (WebKitGTK Sandboxed)")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_html_rendering.set_active(settings.render_html());
            row_html_rendering.connect_active_notify(move |row| {
                settings.set_render_html(row.is_active());
            });
        }
        group_reading.add(&row_html_rendering);

        let row_mail_font_family = adw::EntryRow::builder()
            .title("Schriftfamilie für Mail und Composer")
            .text("system-ui, sans-serif")
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_font_family.set_text(&settings.mail_font_family());
            row_mail_font_family.connect_changed(move |row| {
                settings.set_mail_font_family(&row.text());
            });
        }
        group_reading.add(&row_mail_font_family);

        let row_mail_font_size = adw::SpinRow::with_range(8.0, 48.0, 1.0);
        row_mail_font_size.set_title("Schriftgröße für Mail und Composer");
        row_mail_font_size.set_subtitle("8–48 Punkt");
        row_mail_font_size.set_digits(0);
        row_mail_font_size.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_mail_font_size.set_value(settings.mail_font_size() as f64);
            row_mail_font_size.connect_value_notify(move |row| {
                settings.set_mail_font_size(row.value().round() as u32);
            });
        }
        group_reading.add(&row_mail_font_size);

        let row_mark_read = adw::SwitchRow::builder()
            .title("Nachrichten beim Auswählen als gelesen markieren")
            .subtitle("Die Einstellung gilt für lokale und synchronisierte Mailordner")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mark_read.set_active(settings.mark_read_on_selection());
            row_mark_read.connect_active_notify(move |row| {
                settings.set_mark_read_on_selection(row.is_active());
            });
        }
        group_reading.add(&row_mark_read);

        let row_message_preview = adw::SwitchRow::builder()
            .title("Mail-Vorschaufenster anzeigen")
            .subtitle("Zeigt die ausgewählte Nachricht neben der Nachrichtenliste an")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_message_preview.set_active(settings.show_message_preview());
            row_message_preview.connect_active_notify(move |row| {
                settings.set_show_message_preview(row.is_active());
            });
        }
        group_reading.add(&row_message_preview);

        let row_mail_default_folder = adw::ComboRow::builder()
            .title("Startordner")
            .subtitle("Ordner, der beim Öffnen der Mailansicht zuerst ausgewählt wird")
            .model(&gtk::StringList::new(&[
                "Posteingang",
                "Alle Nachrichten",
                "Gesendet",
                "Entwürfe",
                "Papierkorb",
                "Archiv",
                "Spam",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_default_folder.set_selected(match settings.mail_default_folder().as_str() {
                "all" => 1,
                "sent" => 2,
                "drafts" => 3,
                "trash" => 4,
                "archive" => 5,
                "spam" => 6,
                _ => 0,
            });
            row_mail_default_folder.connect_selected_notify(move |row| {
                settings.set_mail_default_folder(match row.selected() {
                    1 => "all",
                    2 => "sent",
                    3 => "drafts",
                    4 => "trash",
                    5 => "archive",
                    6 => "spam",
                    _ => "inbox",
                });
            });
        }
        group_reading.add(&row_mail_default_folder);

        let row_mail_layout = adw::ComboRow::builder()
            .title("Mail-Arbeitsweise")
            .subtitle(
                "Modern nebeneinander oder klassisch wie Evolution: Ordner links, Liste oben, Vorschau darunter (wird sofort angewendet)",
            )
            .model(&gtk::StringList::new(&[
                "Modern: Liste und Vorschau nebeneinander",
                "Klassisch: Liste über der Vorschau",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_layout.set_selected(match settings.mail_layout().as_str() {
                "classic" => 1,
                _ => 0,
            });
            row_mail_layout.connect_selected_notify(move |row| {
                settings.set_mail_layout(if row.selected() == 1 {
                    "classic"
                } else {
                    "modern"
                });
            });
        }
        group_reading.add(&row_mail_layout);

        let row_mail_density = adw::ComboRow::builder()
            .title("Dichte der Nachrichtenliste")
            .subtitle("Bestimmt den vertikalen Abstand der Mailzeilen")
            .model(&gtk::StringList::new(&[
                "Kompakt",
                "Komfortabel",
                "Großzügig",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_density.set_selected(match settings.mail_row_density().as_str() {
                "compact" => 0,
                "spacious" => 2,
                _ => 1,
            });
            row_mail_density.connect_selected_notify(move |row| {
                settings.set_mail_row_density(match row.selected() {
                    0 => "compact",
                    2 => "spacious",
                    _ => "comfortable",
                });
            });
        }
        group_reading.add(&row_mail_density);

        let row_dark_mail = adw::SwitchRow::builder()
            .title("Dark-Mode für E-Mails erzwingen")
            .subtitle("Passt den Inhalt von HTML-Mails an das dunkle System-Design an")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_dark_mail.set_active(settings.force_dark_mode());
            row_dark_mail.connect_active_notify(move |row| {
                settings.set_force_dark_mode(row.is_active());
            });
        }
        group_reading.add(&row_dark_mail);

        let row_confirm_destructive = adw::SwitchRow::builder()
            .title("Vor destruktiven Aktionen bestätigen")
            .subtitle(
                "Fragt vor dem Löschen von Nachrichten, Terminen, Aufgaben, Kontakten und Notizen",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_confirm_destructive.set_active(settings.confirm_destructive_actions());
            row_confirm_destructive.connect_active_notify(move |row| {
                settings.set_confirm_destructive_actions(row.is_active());
            });
        }
        group_reading.add(&row_confirm_destructive);

        page_appearance.add(&group_reading);

        let group_theme = adw::PreferencesGroup::builder()
            .title("Farbschema")
            .description("Systemeinstellung oder festes helles/dunkles Design")
            .build();
        let row_color_scheme = adw::ComboRow::builder()
            .title("Farbschema der Anwendung")
            .model(&gtk::StringList::new(&["System", "Hell", "Dunkel"]))
            .build();
        if let Some(settings) = settings.clone() {
            row_color_scheme.set_selected(match settings.color_scheme().as_str() {
                "light" => 1,
                "dark" => 2,
                _ => 0,
            });
            row_color_scheme.connect_selected_notify(move |row| {
                let scheme = match row.selected() {
                    1 => "light",
                    2 => "dark",
                    _ => "system",
                };
                settings.set_color_scheme(scheme);
                let color_scheme = match scheme {
                    "light" => adw::ColorScheme::ForceLight,
                    "dark" => adw::ColorScheme::ForceDark,
                    _ => adw::ColorScheme::Default,
                };
                adw::StyleManager::default().set_color_scheme(color_scheme);
            });
        }
        group_theme.add(&row_color_scheme);
        page_appearance.add(&group_theme);

        let group_mail_list = adw::PreferencesGroup::builder()
            .title("Mail-Liste")
            .description("Sortierung der Nachrichten und Unterhaltungen")
            .build();
        let row_mail_sort = adw::ComboRow::builder()
            .title("Sortierfeld")
            .model(&gtk::StringList::new(&[
                "Datum",
                "Betreff",
                "Absender",
                "Ungelesen-Status",
                "Markierung",
                "Wichtigkeit",
                "Anhänge",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sort.set_selected(match settings.mail_sort_field().as_str() {
                "subject" => 1,
                "sender" => 2,
                "unread" => 3,
                "flagged" => 4,
                "important" => 5,
                "attachments" => 6,
                _ => 0,
            });
            row_mail_sort.connect_selected_notify(move |row| {
                settings.set_mail_sort_field(match row.selected() {
                    1 => "subject",
                    2 => "sender",
                    3 => "unread",
                    4 => "flagged",
                    5 => "important",
                    6 => "attachments",
                    _ => "date",
                });
            });
        }
        group_mail_list.add(&row_mail_sort);

        let row_mail_sort_direction = adw::SwitchRow::builder()
            .title("Absteigend sortieren")
            .subtitle("Neueste bzw. alphabetisch letzte Einträge zuerst")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sort_direction.set_active(settings.mail_sort_descending());
            row_mail_sort_direction.connect_active_notify(move |row| {
                settings.set_mail_sort_descending(row.is_active());
            });
        }
        group_mail_list.add(&row_mail_sort_direction);

        let row_mail_threading = adw::SwitchRow::builder()
            .title("Unterhaltungen gruppieren")
            .subtitle("Antwortketten als aufklappbare Threads anzeigen")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_threading.set_active(settings.mail_threading());
            row_mail_threading.connect_active_notify(move |row| {
                settings.set_mail_threading(row.is_active());
            });
        }
        group_mail_list.add(&row_mail_threading);
        page_appearance.add(&group_mail_list);

        let group_mail_columns = adw::PreferencesGroup::builder()
            .title("Mail-Zeile")
            .description("Sichtbare Spalten und Zusatzinformationen der Nachrichtenliste")
            .build();
        let row_mail_show_avatar = adw::SwitchRow::builder()
            .title("Absender-Avatare anzeigen")
            .subtitle("Zeigt Initialen bzw. Bilder links neben dem Absender")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_avatar.set_active(settings.mail_show_avatar());
            row_mail_show_avatar.connect_active_notify(move |row| {
                settings.set_mail_show_avatar(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_avatar);

        let row_mail_show_sender = adw::SwitchRow::builder()
            .title("Absender anzeigen")
            .subtitle("Blendet die Absender-Spalte in der Nachrichtenliste ein oder aus")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_sender.set_active(settings.mail_show_sender());
            row_mail_show_sender.connect_active_notify(move |row| {
                settings.set_mail_show_sender(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_sender);

        let row_mail_show_date = adw::SwitchRow::builder()
            .title("Datum anzeigen")
            .subtitle("Zeigt Datum und Uhrzeit rechts in der Nachrichtenzeile")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_date.set_active(settings.mail_show_date());
            row_mail_show_date.connect_active_notify(move |row| {
                settings.set_mail_show_date(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_date);

        let row_mail_show_preview_text = adw::SwitchRow::builder()
            .title("Vorschautext anzeigen")
            .subtitle("Zeigt den kurzen Nachrichtenauszug unter dem Betreff")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_preview_text.set_active(settings.mail_show_preview_text());
            row_mail_show_preview_text.connect_active_notify(move |row| {
                settings.set_mail_show_preview_text(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_preview_text);

        let row_mail_show_labels = adw::SwitchRow::builder()
            .title("Labels anzeigen")
            .subtitle("Zeigt app-eigene Labels hinter dem Betreff")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_labels.set_active(settings.mail_show_labels());
            row_mail_show_labels.connect_active_notify(move |row| {
                settings.set_mail_show_labels(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_labels);
        page_appearance.add(&group_mail_columns);

        // ── 3. Page: Kalender & Aufgaben ────────────────────────────────────
        let page_calendar = adw::PreferencesPage::builder()
            .title("Kalender")
            .icon_name("x-office-calendar-symbolic")
            .build();

        let group_calendar_view = adw::PreferencesGroup::builder()
            .title("Kalenderansicht")
            .description("Die Auswahl wird beim nächsten Öffnen der Kalenderansicht verwendet")
            .build();
        let row_calendar_view = adw::ComboRow::builder()
            .title("Standardansicht")
            .model(&gtk::StringList::new(&[
                "Tag",
                "Woche",
                "Arbeitswoche",
                "Monat",
                "Agenda",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_calendar_view.set_selected(match settings.calendar_view().as_str() {
                "day" => 0,
                "week" => 1,
                "workweek" => 2,
                "agenda" => 4,
                _ => 3,
            });
            row_calendar_view.connect_selected_notify(move |row| {
                settings.set_calendar_view(match row.selected() {
                    0 => "day",
                    1 => "week",
                    2 => "workweek",
                    4 => "agenda",
                    _ => "month",
                });
            });
        }
        group_calendar_view.add(&row_calendar_view);

        let row_week_start = adw::ComboRow::builder()
            .title("Wochenbeginn")
            .model(&gtk::StringList::new(&["Montag", "Sonntag"]))
            .build();
        if let Some(settings) = settings.clone() {
            row_week_start.set_selected(if settings.calendar_week_start() == "sunday" {
                1
            } else {
                0
            });
            row_week_start.connect_selected_notify(move |row| {
                settings.set_calendar_week_start(if row.selected() == 1 {
                    "sunday"
                } else {
                    "monday"
                });
            });
        }
        group_calendar_view.add(&row_week_start);

        let row_week_numbers = adw::SwitchRow::builder()
            .title("Kalenderwochen anzeigen")
            .subtitle("ISO-Kalenderwochen im Monatsraster und in der Wochenansicht anzeigen")
            .build();
        if let Some(settings) = settings.clone() {
            row_week_numbers.set_active(settings.calendar_show_week_numbers());
            row_week_numbers.connect_active_notify(move |row| {
                settings.set_calendar_show_week_numbers(row.is_active());
            });
        }
        group_calendar_view.add(&row_week_numbers);

        let row_date_format = adw::ComboRow::builder()
            .title("Datumsformat")
            .subtitle("Darstellung in Kalenderübersichten und Agenda")
            .model(&gtk::StringList::new(&[
                "Deutsch (z. B. 11.08.2026)",
                "ISO (2026-08-11)",
                "US (08/11/2026)",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_date_format.set_selected(match settings.date_format().as_str() {
                "iso" => 1,
                "us" => 2,
                _ => 0,
            });
            row_date_format.connect_selected_notify(move |row| {
                settings.set_date_format(match row.selected() {
                    1 => "iso",
                    2 => "us",
                    _ => "german",
                });
            });
        }
        group_calendar_view.add(&row_date_format);

        let row_time_format = adw::ComboRow::builder()
            .title("Zeitformat")
            .subtitle("Zeitachse und Terminzeiten")
            .model(&gtk::StringList::new(&[
                "24-Stunden-Format",
                "12-Stunden-Format",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_time_format.set_selected(if settings.time_format() == "12h" {
                1
            } else {
                0
            });
            row_time_format.connect_selected_notify(move |row| {
                settings.set_time_format(if row.selected() == 1 { "12h" } else { "24h" });
            });
        }
        group_calendar_view.add(&row_time_format);
        page_calendar.add(&group_calendar_view);

        let group_default_ecal_sources = adw::PreferencesGroup::builder()
            .title("Standardquellen")
            .description("Quellen für neue Termine, Aufgaben und Notizen")
            .build();
        group_default_ecal_sources.add(&source_preference_row(
            "Standardkalender",
            "Neue Termine werden hier angelegt",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Lokaler Kalender",
            settings.clone(),
            AppSettings::default_calendar_source_uid,
            AppSettings::set_default_calendar_source_uid,
        ));
        group_default_ecal_sources.add(&source_preference_row(
            "Standardaufgabenliste",
            "Neue Aufgaben werden hier angelegt",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Lokale Aufgabenliste",
            settings.clone(),
            AppSettings::default_task_source_uid,
            AppSettings::set_default_task_source_uid,
        ));
        group_default_ecal_sources.add(&source_preference_row(
            "Standardnotizliste",
            "Neue Notizen werden hier angelegt",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Lokale Notizliste",
            settings.clone(),
            AppSettings::default_memo_source_uid,
            AppSettings::set_default_memo_source_uid,
        ));
        page_calendar.add(&group_default_ecal_sources);

        let group_calendar_days = adw::PreferencesGroup::builder()
            .title("Arbeitstage")
            .description("Diese Wochentage werden in der Arbeitswochenansicht angezeigt")
            .build();
        for (day, label) in [
            ("monday", "Montag"),
            ("tuesday", "Dienstag"),
            ("wednesday", "Mittwoch"),
            ("thursday", "Donnerstag"),
            ("friday", "Freitag"),
            ("saturday", "Samstag"),
            ("sunday", "Sonntag"),
        ] {
            let row = adw::SwitchRow::builder().title(label).build();
            if let Some(settings) = settings.clone() {
                let configured_days = settings.calendar_workdays();
                row.set_active(configured_days.split(',').any(|candidate| candidate == day));
                let day = day.to_string();
                row.connect_active_notify(move |row| {
                    let configured_days = settings.calendar_workdays();
                    let mut days = configured_days
                        .split(',')
                        .filter(|candidate| !candidate.is_empty() && *candidate != day)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                    if row.is_active() {
                        days.push(day.clone());
                    }
                    settings.set_calendar_workdays(&days.join(","));
                });
            }
            group_calendar_days.add(&row);
        }
        page_calendar.add(&group_calendar_days);

        let group_calendar_time = adw::PreferencesGroup::builder()
            .title("Arbeitszeit &amp; Erinnerungen")
            .description("Zeiten werden als Minuten nach Mitternacht gespeichert; 0 Minuten deaktiviert Erinnerungen")
            .build();
        let row_workday_start = adw::SpinRow::with_range(0.0, 1_439.0, 15.0);
        row_workday_start.set_title("Arbeitszeit beginnt");
        row_workday_start.set_subtitle("Minuten nach Mitternacht (z. B. 540 = 09:00)");
        row_workday_start.set_digits(0);
        row_workday_start.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_workday_start.set_value(settings.calendar_workday_start_minutes() as f64);
            row_workday_start.connect_value_notify(move |row| {
                settings.set_calendar_workday_start_minutes(row.value().round() as u32);
            });
        }
        group_calendar_time.add(&row_workday_start);

        let row_workday_end = adw::SpinRow::with_range(0.0, 1_439.0, 15.0);
        row_workday_end.set_title("Arbeitszeit endet");
        row_workday_end.set_subtitle("Minuten nach Mitternacht (z. B. 1.020 = 17:00)");
        row_workday_end.set_digits(0);
        row_workday_end.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_workday_end.set_value(settings.calendar_workday_end_minutes() as f64);
            row_workday_end.connect_value_notify(move |row| {
                settings.set_calendar_workday_end_minutes(row.value().round() as u32);
            });
        }
        group_calendar_time.add(&row_workday_end);

        let row_default_reminder = adw::SpinRow::with_range(0.0, 10_080.0, 5.0);
        row_default_reminder.set_title("Standard-Erinnerung");
        row_default_reminder.set_subtitle("Minuten vor einem neuen Termin");
        row_default_reminder.set_digits(0);
        row_default_reminder.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_default_reminder.set_value(settings.calendar_default_reminder_minutes() as f64);
            row_default_reminder.connect_value_notify(move |row| {
                settings.set_calendar_default_reminder_minutes(row.value().round() as u32);
            });
        }
        group_calendar_time.add(&row_default_reminder);

        let row_notifications = adw::SwitchRow::builder()
            .title("Desktop-Benachrichtigungen")
            .subtitle(
                "Lokale Termin- und Aufgaben-Erinnerungen als GNOME-Benachrichtigung anzeigen",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_notifications.set_active(settings.notifications_enabled());
            row_notifications.connect_active_notify(move |row| {
                settings.set_notifications_enabled(row.is_active());
            });
        }
        group_calendar_time.add(&row_notifications);
        page_calendar.add(&group_calendar_time);

        let group_categories = adw::PreferencesGroup::builder()
            .title("Kalenderkategorien")
            .description(
                "Kategoriebezeichnungen werden im iCalendar-Datensatz gespeichert; Farben bleiben app-eigene Präsentationsdaten",
            )
            .build();
        let category_rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        for category in CalendarCategoryStore::default().load() {
            append_category_editor_row(&category_rows, self, Some(category));
        }
        let add_category_button = gtk::Button::builder()
            .label("Kategorie hinzufügen")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["flat"])
            .halign(gtk::Align::Start)
            .build();
        let category_rows_for_add = category_rows.clone();
        let dialog_weak_for_add = self.downgrade();
        add_category_button.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak_for_add.upgrade() else {
                return;
            };
            append_category_editor_row(&category_rows_for_add, &dialog, None);
        });
        group_categories.add(&category_rows);
        group_categories.add(&add_category_button);
        page_calendar.add(&group_categories);

        let group_task_memo_lists = adw::PreferencesGroup::builder()
            .title("Aufgaben- und Notizlisten")
            .description("Filterung und Sortierung der Aufgaben- und Memo-Ansichten")
            .build();

        let row_task_filter = adw::ComboRow::builder()
            .title("Aufgabenfilter")
            .model(&gtk::StringList::new(&[
                "Alle Aufgaben",
                "Heute fällig",
                "Wichtig",
                "Überfällig",
                "In Bearbeitung",
                "Abgebrochen",
                "Erledigt",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_task_filter.set_selected(match settings.task_filter().as_str() {
                "today" => 1,
                "important" => 2,
                "overdue" => 3,
                "in-progress" => 4,
                "cancelled" => 5,
                "completed" => 6,
                _ => 0,
            });
            row_task_filter.connect_selected_notify(move |row| {
                settings.set_task_filter(match row.selected() {
                    1 => "today",
                    2 => "important",
                    3 => "overdue",
                    4 => "in-progress",
                    5 => "cancelled",
                    6 => "completed",
                    _ => "all",
                });
            });
        }
        group_task_memo_lists.add(&row_task_filter);

        let row_task_sort = adw::ComboRow::builder()
            .title("Aufgaben sortieren nach")
            .model(&gtk::StringList::new(&["Fälligkeit", "Titel", "Priorität"]))
            .build();
        if let Some(settings) = settings.clone() {
            row_task_sort.set_selected(match settings.task_sort_field().as_str() {
                "title" => 1,
                "priority" => 2,
                _ => 0,
            });
            row_task_sort.connect_selected_notify(move |row| {
                settings.set_task_sort_field(match row.selected() {
                    1 => "title",
                    2 => "priority",
                    _ => "due",
                });
            });
        }
        group_task_memo_lists.add(&row_task_sort);

        let row_task_sort_direction = adw::SwitchRow::builder()
            .title("Aufgaben absteigend sortieren")
            .subtitle("Spätere Fälligkeiten, Titel oder höhere Prioritäten zuerst")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_task_sort_direction.set_active(settings.task_sort_descending());
            row_task_sort_direction.connect_active_notify(move |row| {
                settings.set_task_sort_descending(row.is_active());
            });
        }
        group_task_memo_lists.add(&row_task_sort_direction);

        let row_memo_sort = adw::ComboRow::builder()
            .title("Notizen sortieren nach")
            .model(&gtk::StringList::new(&[
                "Änderungszeitpunkt",
                "Titel",
                "Quelle",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_memo_sort.set_selected(match settings.memo_sort_field().as_str() {
                "title" => 1,
                "source" => 2,
                _ => 0,
            });
            row_memo_sort.connect_selected_notify(move |row| {
                settings.set_memo_sort_field(match row.selected() {
                    1 => "title",
                    2 => "source",
                    _ => "updated",
                });
            });
        }
        group_task_memo_lists.add(&row_memo_sort);

        let row_memo_sort_direction = adw::SwitchRow::builder()
            .title("Notizen absteigend sortieren")
            .subtitle("Neueste Änderungen, Titel oder Quellen zuerst")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_memo_sort_direction.set_active(settings.memo_sort_descending());
            row_memo_sort_direction.connect_active_notify(move |row| {
                settings.set_memo_sort_descending(row.is_active());
            });
        }
        group_task_memo_lists.add(&row_memo_sort_direction);
        page_calendar.add(&group_task_memo_lists);

        // ── 4. Page: Sicherheit & PGP/S-MIME ───────────────────────────────
        let page_security = adw::PreferencesPage::builder()
            .title("Sicherheit")
            .icon_name("security-high-symbolic")
            .build();

        let group_crypto = adw::PreferencesGroup::builder()
            .title("Verschlüsselung &amp; Signaturen")
            .description("OpenPGP und S/MIME Konfiguration (CamelCipherContext)")
            .build();

        let row_pgp_encrypt = adw::SwitchRow::builder()
            .title("Neue Mails standardmäßig PGP-signieren")
            .subtitle("Verwendet GnuPG Schlüsselbund")
            .active(true)
            .build();
        group_crypto.add(&row_pgp_encrypt);

        let row_pgp_key = adw::ComboRow::builder()
            .title("Standard PGP-Schlüssel")
            .model(&gtk::StringList::new(&[
                "Kein lokaler PGP-Schlüssel verfügbar",
                "Automatisch anhand der Absenderadresse wählen",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_pgp_key.set_selected(if settings.default_pgp_key().is_empty() {
                1
            } else {
                0
            });
            row_pgp_key.connect_selected_notify(move |row| {
                if row.selected() == 1 {
                    settings.set_default_pgp_key("");
                }
            });
        }
        if let Some(settings) = settings.clone() {
            row_pgp_encrypt.set_active(settings.sign_new_messages());
            row_pgp_encrypt.connect_active_notify(move |row| {
                settings.set_sign_new_messages(row.is_active());
            });
        }
        group_crypto.add(&row_pgp_key);

        page_security.add(&group_crypto);

        // Add pages to window
        self.add(&page_accounts);
        self.add(&page_appearance);
        self.add(&page_calendar);
        self.add(&page_security);
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Preferences dialog result");
    }
}

fn source_preference_row(
    title: &str,
    subtitle: &str,
    kind: EdsSourceKind,
    local_uid: &str,
    local_label: &str,
    settings: Option<AppSettings>,
    get_source: fn(&AppSettings) -> String,
    set_source: fn(&AppSettings, &str),
) -> adw::ComboRow {
    let source_model = gtk::StringList::new(&["Quellen werden geladen…"]);
    let source_uids = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let row = adw::ComboRow::builder()
        .title(title)
        .subtitle(subtitle)
        .model(&source_model)
        .build();

    let source_uids_for_selection = Rc::clone(&source_uids);
    let settings_for_selection = settings.clone();
    row.connect_selected_notify(move |row| {
        let Some(settings) = settings_for_selection.as_ref() else {
            return;
        };
        let Some(source_uid) = source_uids_for_selection
            .borrow()
            .get(row.selected() as usize)
            .cloned()
        else {
            return;
        };
        set_source(settings, &source_uid);
    });

    let preferred_source_uid = settings
        .as_ref()
        .map(get_source)
        .filter(|uid| !uid.trim().is_empty());
    let source_model_for_load = source_model.clone();
    let source_uids_for_load = Rc::clone(&source_uids);
    let row_for_load = row.clone();
    let local_uid = local_uid.to_string();
    let local_label = local_label.to_string();
    let local_available = match kind {
        EdsSourceKind::Calendar => CalendarStoreService::new().is_writable(),
        EdsSourceKind::AddressBook => ContactStoreService::new().is_writable(),
        EdsSourceKind::Mail => false,
    };
    let subtitle = subtitle.to_string();
    glib::spawn_future_local(async move {
        let discovery = eds_service::discover_sources(kind).await;
        let discovery_status = discovery.status_text();
        let mut sources = discovery.sources;
        sources.retain(|source| source.enabled);
        sources
            .sort_by_cached_key(|source| (source.display_name.to_lowercase(), source.uid.clone()));

        let mut names = sources
            .iter()
            .map(|source| {
                if source.backend_name.trim().is_empty() {
                    source.display_name.clone()
                } else {
                    format!("{} · {}", source.display_name, source.backend_name)
                }
            })
            .collect::<Vec<_>>();
        let mut uids = sources
            .into_iter()
            .map(|source| source.uid)
            .collect::<Vec<_>>();

        if let Some(preferred_source_uid) = preferred_source_uid.as_deref()
            && !uids.iter().any(|uid| uid == preferred_source_uid)
        {
            names.insert(
                0,
                format!("Nicht verfügbare Quelle · {preferred_source_uid}"),
            );
            uids.insert(0, preferred_source_uid.to_string());
        }
        if local_available {
            names.push(local_label);
            uids.push(local_uid.clone());
        }

        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
        source_uids_for_load.replace(uids.clone());
        row_for_load.set_subtitle(&format!("{subtitle} · {discovery_status}"));

        let selected_index = preferred_source_uid
            .as_deref()
            .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
            .or_else(|| {
                local_available
                    .then(|| uids.iter().position(|uid| uid == &local_uid))
                    .flatten()
            });
        if let Some(selected_index) = selected_index {
            row_for_load.set_selected(selected_index as u32);
        }
    });

    row
}

fn directory_picker_row<F>(
    parent: &PreferencesDialog,
    title: &str,
    default_subtitle: &str,
    initial_path: String,
    settings: Option<AppSettings>,
    set_path: F,
) -> adw::ActionRow
where
    F: Fn(&AppSettings, &str) + 'static,
{
    let default_subtitle = default_subtitle.to_string();
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(&default_subtitle)
        .build();
    if !initial_path.trim().is_empty() {
        row.set_subtitle(&initial_path);
    }

    let choose_button = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Ordner auswählen")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text("Automatische Auswahl wiederherstellen")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&choose_button);
    row.add_suffix(&clear_button);

    let set_path: Rc<dyn Fn(&AppSettings, &str)> = Rc::new(set_path);
    let dialog_weak = parent.downgrade();
    let row_for_choose = row.clone();
    let settings_for_choose = settings.clone();
    let set_path_for_choose = Rc::clone(&set_path);
    choose_button.connect_clicked(move |_| {
        let Some(dialog) = dialog_weak.upgrade() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("Lokalen Importordner auswählen")
            .modal(true)
            .build();
        let parent_window = dialog
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let settings_for_result = settings_for_choose.clone();
        let set_path_for_result = Rc::clone(&set_path_for_choose);
        let row_for_result = row_for_choose.clone();
        file_dialog.select_folder(
            parent_window.as_ref(),
            None::<&gio::Cancellable>,
            move |result| {
                let Ok(file) = result else {
                    return;
                };
                let Some(path) = file.path() else {
                    return;
                };
                let path = path.to_string_lossy().into_owned();
                if let Some(settings) = settings_for_result.as_ref() {
                    set_path_for_result(settings, &path);
                }
                row_for_result.set_subtitle(&path);
            },
        );
    });

    let row_for_clear = row.clone();
    let settings_for_clear = settings;
    clear_button.connect_clicked(move |_| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_path(settings, "");
        }
        row_for_clear.set_subtitle(&default_subtitle);
    });

    row
}

fn append_category_editor_row(
    container: &gtk::Box,
    parent: &PreferencesDialog,
    category: Option<CalendarCategory>,
) {
    let category_id = Rc::new(std::cell::RefCell::new(
        category.as_ref().map(|category| category.id.clone()),
    ));
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(2)
        .margin_bottom(2)
        .build();
    let name_entry = gtk::Entry::builder()
        .placeholder_text("Name, z. B. Arbeit")
        .hexpand(true)
        .build();
    let color_entry = gtk::Entry::builder()
        .placeholder_text("Farbe, z. B. #729FCF")
        .width_chars(10)
        .build();
    if let Some(category) = category.as_ref() {
        name_entry.set_text(&category.name);
        color_entry.set_text(&category.color);
    } else {
        color_entry.set_text("#729FCF");
    }
    let save_button = gtk::Button::builder()
        .label("Speichern")
        .css_classes(vec!["flat", "suggested-action"])
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Kategorie löschen")
        .css_classes(vec!["flat", "error"])
        .build();
    row.append(&name_entry);
    row.append(&color_entry);
    row.append(&save_button);
    row.append(&delete_button);
    container.append(&row);

    let parent_weak_for_save = parent.downgrade();
    let category_id_for_save = Rc::clone(&category_id);
    let name_for_save = name_entry.clone();
    let color_for_save = color_entry.clone();
    save_button.connect_clicked(move |_| {
        let id = category_id_for_save
            .borrow()
            .clone()
            .unwrap_or_else(|| format!("category-{}", uuid::Uuid::new_v4()));
        let category = CalendarCategory {
            id: id.clone(),
            name: name_for_save.text().to_string(),
            color: color_for_save.text().to_string(),
        };
        match CalendarCategoryStore::default().upsert(category) {
            Ok(()) => {
                category_id_for_save.replace(Some(id));
                if let Some(parent) = parent_weak_for_save.upgrade() {
                    parent.show_toast("Kalenderkategorie gespeichert");
                }
            }
            Err(error) => {
                if let Some(parent) = parent_weak_for_save.upgrade() {
                    parent.show_toast(&format!(
                        "Kategorie konnte nicht gespeichert werden: {error}"
                    ));
                }
            }
        }
    });

    let parent_weak_for_delete = parent.downgrade();
    let category_id_for_delete = Rc::clone(&category_id);
    let row_for_delete = row.clone();
    let container_for_delete = container.clone();
    delete_button.connect_clicked(move |button| {
        let Some(_parent) = parent_weak_for_delete.upgrade() else {
            return;
        };
        let Some(id) = category_id_for_delete.borrow().clone() else {
            container_for_delete.remove(&row_for_delete);
            return;
        };
        let row = row_for_delete.clone();
        let container = container_for_delete.clone();
        let parent_weak_for_result = parent_weak_for_delete.clone();
        crate::dialogs::run_destructive_action(
            button,
            "Kalenderkategorie löschen?",
            "Die Kategorie wird aus den app-eigenen Farbvoreinstellungen entfernt. Bereits gespeicherte iCalendar-Kategorien bleiben erhalten.",
            "Löschen",
            move || match CalendarCategoryStore::default().delete(&id) {
                Ok(true) => {
                    container.remove(&row);
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast("Kalenderkategorie gelöscht");
                    }
                }
                Ok(false) => {
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast("Kalenderkategorie wurde nicht gefunden");
                    }
                }
                Err(error) => {
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast(&format!("Kategorie konnte nicht gelöscht werden: {error}"));
                    }
                }
            },
        );
    });
}

fn selected_file_paths(model: &gio::ListModel) -> Vec<std::path::PathBuf> {
    (0..model.n_items())
        .filter_map(|index| {
            model
                .item(index)
                .and_downcast::<gio::File>()
                .and_then(|file| file.path())
        })
        .collect()
}

fn filter_field_label(field: FilterField) -> &'static str {
    match field {
        FilterField::Subject => "Betreff",
        FilterField::Sender => "Absender",
        FilterField::Recipient => "Empfänger",
        FilterField::Body => "Nachricht",
        FilterField::Header => "Header",
        FilterField::Label => "Label",
    }
}

fn filter_operator_label(operator: FilterOperator) -> &'static str {
    match operator {
        FilterOperator::Contains => "enthält",
        FilterOperator::NotContains => "enthält nicht",
        FilterOperator::Equals => "ist",
        FilterOperator::NotEquals => "ist nicht",
        FilterOperator::StartsWith => "beginnt mit",
        FilterOperator::EndsWith => "endet mit",
        FilterOperator::Exists => "existiert",
    }
}

fn filter_action_label(action: &FilterAction) -> String {
    match action {
        FilterAction::MoveToFolder(folder) => format!("verschieben nach {folder}"),
        FilterAction::CopyToFolder(folder) => format!("kopieren nach {folder}"),
        FilterAction::MarkRead => "als gelesen markieren".to_string(),
        FilterAction::MarkUnread => "als ungelesen markieren".to_string(),
        FilterAction::SetFlagged => "Wichtig-Flag setzen".to_string(),
        FilterAction::ClearFlagged => "Wichtig-Flag entfernen".to_string(),
        FilterAction::Delete => "löschen".to_string(),
        FilterAction::StopProcessing => "weitere Filterregeln nicht ausführen".to_string(),
        FilterAction::SetJunk => "als Spam markieren".to_string(),
        FilterAction::ClearJunk => "als kein Spam markieren".to_string(),
        FilterAction::SetLabel(label) => format!("Label setzen „{label}“"),
        FilterAction::ClearLabel(label) => format!("Label entfernen „{label}“"),
    }
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}
