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
use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::services::AppSettings;
use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, AccountProtocol, reconciled_local_default_account,
};
use crate::services::backup_service;
use crate::services::calendar_category_service::{CalendarCategory, CalendarCategoryStore};
use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::crypto_service::{self, OpenPgpKey};
use crate::services::eds_service::{self, EdsSourceKind};
use crate::services::eds_source_cache_service::EdsSourceCache;
use crate::services::mail_filter_service::{
    FilterAction, FilterField, FilterOperator, MailFilterRule, MailFilterStore,
};
use crate::services::mail_store_service::{
    LocalFilterRunReport, MailStoreService, MailStoreWorkerConfig,
};
use crate::services::migration_service;
use crate::services::settings_service::{ReminderDurationUnit, validate_mail_normal_header_name};
use crate::services::signature_store_service::SignatureStore;

/// Owned, framework-neutral inputs for one manual filter worker.
///
/// The selected rule is materialized into a private, one-rule store before
/// the worker starts.  This is necessary because the service API accepts a
/// rule ID and otherwise reads the mutable filter file again on the worker.
/// The worker therefore receives only owned values and cannot observe later
/// settings, environment, or filter-file changes.
#[derive(Debug)]
struct ManualFilterRunSnapshot {
    rule_id: String,
    rule: MailFilterRule,
    store_config: MailStoreWorkerConfig,
    filter_store: MailFilterStore,
}

#[derive(Debug)]
enum ManualFilterSnapshotError {
    MissingRule,
    InvalidRule(String),
    Io(io::Error),
}

impl ManualFilterSnapshotError {
    fn toast_message(&self) -> String {
        match self {
            Self::MissingRule => "Filterregel wurde nicht gefunden".to_string(),
            Self::InvalidRule(error) => {
                format!("Lokaler Filterlauf fehlgeschlagen: Ungültige Filterregel: {error}")
            }
            Self::Io(error) => format!(
                "Lokaler Filterlauf fehlgeschlagen: Filterregel-Snapshot konnte nicht vorbereitet werden: {error}"
            ),
        }
    }
}

fn manual_filter_snapshot_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "evolution-gtk4-manual-filter-{}.json",
        uuid::Uuid::new_v4()
    ))
}

fn cleanup_manual_filter_snapshot(path: &Path) {
    let temporary_path = path.with_extension("json.tmp");
    for candidate in [path, temporary_path.as_path()] {
        match fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::debug!(
                path = %candidate.display(),
                %error,
                "Could not remove manual filter snapshot"
            ),
        }
    }
}

struct ManualFilterSnapshotCleanup {
    path: PathBuf,
}

impl Drop for ManualFilterSnapshotCleanup {
    fn drop(&mut self) {
        cleanup_manual_filter_snapshot(&self.path);
    }
}

impl ManualFilterRunSnapshot {
    fn capture(
        rule_id: impl Into<String>,
        rule: MailFilterRule,
        store_config: MailStoreWorkerConfig,
        source_filter_store: MailFilterStore,
    ) -> Result<Self, ManualFilterSnapshotError> {
        let rule_id = rule_id.into();
        if rule.id != rule_id {
            return Err(ManualFilterSnapshotError::InvalidRule(
                "Die ausgewählte Regel-ID stimmt nicht mit dem Regel-Snapshot überein".to_string(),
            ));
        }
        if !source_filter_store
            .load()
            .into_iter()
            .any(|candidate| candidate.id == rule_id)
        {
            return Err(ManualFilterSnapshotError::MissingRule);
        }
        rule.validate()
            .map_err(ManualFilterSnapshotError::InvalidRule)?;

        let filter_store = MailFilterStore::new(manual_filter_snapshot_path());
        if let Err(error) = filter_store.save(std::slice::from_ref(&rule)) {
            cleanup_manual_filter_snapshot(filter_store.path());
            return Err(ManualFilterSnapshotError::Io(error));
        }

        Ok(Self {
            rule_id,
            rule,
            store_config,
            filter_store,
        })
    }

    fn run_on_worker(self) -> std::io::Result<Option<LocalFilterRunReport>> {
        let _cleanup = ManualFilterSnapshotCleanup {
            path: self.filter_store.path().to_path_buf(),
        };
        let Self {
            rule_id,
            rule,
            store_config,
            filter_store,
        } = self;

        // MailFilterStore::load intentionally turns malformed/unreadable
        // files into an empty rule list.  Reject a missing or changed private
        // snapshot here so the UI cannot report a misleading zero-match
        // success.
        let Some(snapshot_rule) = filter_store
            .load()
            .into_iter()
            .find(|candidate| candidate.id == rule_id)
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Der Regel-Snapshot fehlt oder ist nicht lesbar",
            ));
        };
        if snapshot_rule != rule {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Der Regel-Snapshot wurde verändert",
            ));
        }

        MailStoreService::new_worker_safe(store_config)
            .run_filter_rule_with_store(&rule_id, &filter_store)
    }
}

fn manual_filter_result_toast(
    result: &Result<io::Result<Option<LocalFilterRunReport>>, async_channel::RecvError>,
) -> String {
    match result {
        Ok(Ok(Some(report))) => format!(
            "{} Treffer · {} geändert · {} verschoben · {} kopiert · {} gelöscht",
            report.matched_messages,
            report.changed_messages,
            report.moved_messages,
            report.copied_messages,
            report.deleted_messages,
        ),
        Ok(Ok(None)) => "Filterregel wurde nicht gefunden".to_string(),
        Ok(Err(error)) => format!("Lokaler Filterlauf fehlgeschlagen: {error}"),
        Err(error) => format!("Lokaler Filterlauf abgebrochen: {error}"),
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct PreferencesDialog {
        pub local_profile_group: RefCell<Option<adw::PreferencesGroup>>,
        pub local_profile_rows: RefCell<Vec<adw::PreferencesRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "EvolutionPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_content_width(980);
            self.obj().set_content_height(720);
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreferencesDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn present_account_dialog(&self, dialog: crate::dialogs::AccountSetupDialog) {
        if let Some(window) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&window));
        }
        let page = self.downgrade();
        dialog.connect_close_request(move |_| {
            if let Some(page) = page.upgrade() {
                page.refresh_local_profile_rows();
                if let Some(window) = page.root().and_downcast::<crate::window::EvolutionWindow>() {
                    window.refresh_mail_page();
                }
            }
            glib::Propagation::Proceed
        });
        dialog.present();
    }

    fn reconcile_local_default(&self, affected_local_id: &str) {
        let Some(settings) = AppSettings::try_new() else {
            return;
        };
        let current = settings.default_account_id();
        let profiles = AccountConfigStore::default().load();
        let reconciled = reconciled_local_default_account(&current, affected_local_id, &profiles);
        if reconciled != current {
            settings.set_default_account_id(&reconciled);
        }
    }

    fn refresh_local_profile_rows(&self) {
        let Some(group) = self.imp().local_profile_group.borrow().as_ref().cloned() else {
            return;
        };
        for row in self.imp().local_profile_rows.borrow_mut().drain(..) {
            group.remove(&row);
        }

        let profiles = AccountConfigStore::default().load();
        let default_id = AppSettings::try_new()
            .map(|settings| settings.default_account_id())
            .unwrap_or_default();
        if profiles.is_empty() {
            let row = adw::ActionRow::builder()
                .title("Keine lokalen Kontoprofile")
                .subtitle("Über „Neues Konto hinzufügen…“ ein Profil anlegen")
                .build();
            group.add(&row);
            self.imp()
                .local_profile_rows
                .borrow_mut()
                .push(row.upcast());
            return;
        }

        let profile_count = profiles.len();
        for (index, profile) in profiles.into_iter().enumerate() {
            let is_default = profile.enabled && profile.id == default_id;
            let mut extras = Vec::new();
            if is_default {
                extras.push("Standardkonto");
            }
            if let Some(ref sig) = profile.signature_id {
                if !sig.is_empty() {
                    extras.push("Signatur konfiguriert");
                }
            }
            if !profile.pgp_key_id.is_empty() {
                extras.push("PGP");
            }
            if profile.auto_check_enabled {
                extras.push("Auto-Abruf");
            }
            let extra_str = if extras.is_empty() {
                String::new()
            } else {
                format!(" · {}", extras.join(", "))
            };
            let subtitle = format!(
                "{} · {}{extra_str}",
                profile.email,
                profile.protocol.as_str()
            );
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
                    page.present_account_dialog(crate::dialogs::AccountSetupDialog::new_for_edit(
                        &profile_for_edit,
                    ));
                }
            ));

            let edit_btn = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Benutzerdefinierte Kontoeinstellungen bearbeiten")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let profile_for_edit_btn = profile.clone();
            edit_btn.connect_clicked(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    page.present_account_dialog(crate::dialogs::AccountSetupDialog::new_for_edit(
                        &profile_for_edit_btn,
                    ));
                }
            ));
            row.add_suffix(&edit_btn);

            let move_up = gtk::Button::builder()
                .icon_name("go-up-symbolic")
                .tooltip_text("Profil nach oben verschieben")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .sensitive(index > 0)
                .build();
            let profile_id = profile.id.clone();
            let page = self.downgrade();
            move_up.connect_clicked(move |_| {
                let result = AccountConfigStore::default().move_profile(&profile_id, -1);
                if let Some(page) = page.upgrade() {
                    match result {
                        Ok(true) => page.refresh_local_profile_rows(),
                        Ok(false) => page.show_toast("Kontoprofil wurde nicht gefunden"),
                        Err(error) => page.show_toast(&format!(
                            "Kontoreihenfolge konnte nicht gespeichert werden: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&move_up);

            let move_down = gtk::Button::builder()
                .icon_name("go-down-symbolic")
                .tooltip_text("Profil nach unten verschieben")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .sensitive(index + 1 < profile_count)
                .build();
            let profile_id = profile.id.clone();
            let page = self.downgrade();
            move_down.connect_clicked(move |_| {
                let result = AccountConfigStore::default().move_profile(&profile_id, 1);
                if let Some(page) = page.upgrade() {
                    match result {
                        Ok(true) => page.refresh_local_profile_rows(),
                        Ok(false) => page.show_toast("Kontoprofil wurde nicht gefunden"),
                        Err(error) => page.show_toast(&format!(
                            "Kontoreihenfolge konnte nicht gespeichert werden: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&move_down);

            let enabled = gtk::Switch::builder()
                .active(profile.enabled)
                .valign(gtk::Align::Center)
                .tooltip_text("Profil aktivieren/deaktivieren")
                .build();
            let profile_id = profile.id.clone();
            let page = self.downgrade();
            enabled.connect_active_notify(move |switch| {
                let result =
                    AccountConfigStore::default().set_enabled(&profile_id, switch.is_active());
                if let Some(page) = page.upgrade() {
                    match result {
                        Ok(true) => {
                            page.reconcile_local_default(&profile_id);
                            page.refresh_local_profile_rows();
                        }
                        Ok(false) => page.show_toast("Kontoprofil wurde nicht gefunden"),
                        Err(error) => page.show_toast(&format!(
                            "Kontoprofil konnte nicht aktualisiert werden: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&enabled);

            let default_button = gtk::Button::builder()
                .icon_name(if is_default {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if is_default {
                    "Aktuelles Standardkonto"
                } else {
                    "Als Standardkonto für neue Nachrichten verwenden"
                })
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .sensitive(profile.enabled)
                .build();
            let profile_id = profile.id.clone();
            let page = self.downgrade();
            default_button.connect_clicked(move |_| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_default_account_id(&profile_id);
                }
                if let Some(page) = page.upgrade() {
                    page.show_toast("Standardkonto gespeichert");
                    page.refresh_local_profile_rows();
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
            let page = self.downgrade();
            let profile_id = profile.id.clone();
            delete_button.connect_clicked(move |_| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                let page_for_action = page.clone();
                let profile_id = profile_id.clone();
                crate::dialogs::run_destructive_action(
                    &page,
                    "Lokales Kontoprofil entfernen?",
                    "Nur das app-eigene, geheimnisfreie Profil wird entfernt. Konten und sichere Zugangsdaten bleiben unverändert.",
                    "Entfernen",
                    move || match AccountConfigStore::default().delete(&profile_id) {
                        Ok(true) => {
                            page_for_action.reconcile_local_default(&profile_id);
                            page_for_action.refresh_local_profile_rows();
                            page_for_action.show_toast("Lokales Kontoprofil entfernt");
                        }
                        Ok(false) => {
                            page_for_action.show_toast("Lokales Kontoprofil wurde nicht gefunden")
                        }
                        Err(error) => page_for_action.show_toast(&format!(
                            "Kontoprofil konnte nicht entfernt werden: {error}"
                        )),
                    },
                );
            });

            group.add(&row);
            self.imp()
                .local_profile_rows
                .borrow_mut()
                .push(row.upcast());
        }
    }

    fn build_ui(&self) {
        let settings = AppSettings::try_new();

        // ── 9 Sidebar Pages matching Original Evolution ───────────────────────
        let page_accounts = adw::PreferencesPage::builder()
            .title("Mail Accounts")
            .icon_name("avatar-default-symbolic")
            .build();

        let page_contacts = adw::PreferencesPage::builder()
            .title("Contacts")
            .icon_name("contact-new-symbolic")
            .build();

        let page_network = adw::PreferencesPage::builder()
            .title("Network Preferences")
            .icon_name("network-wired-symbolic")
            .build();

        let page_backup = adw::PreferencesPage::builder()
            .title("Import &amp; Backup")
            .icon_name("drive-harddisk-symbolic")
            .build();

        let group_accounts = adw::PreferencesGroup::builder()
            .title("E-Mail-Konten &amp; Server-Einstellungen")
            .description("Verwaltete Konten und Online-Dienste")
            .build();

        let row_add_account = adw::ActionRow::builder()
            .title("Neues Konto hinzufügen…")
            .subtitle("IMAP, POP3, Exchange (EWS) oder ein Onlinekonto einrichten")
            .activatable(true)
            .build();
        let icon_add = gtk::Image::from_icon_name("list-add-symbolic");
        row_add_account.add_prefix(&icon_add);

        row_add_account.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.present_account_dialog(crate::dialogs::AccountSetupDialog::new());
            }
        ));
        group_accounts.add(&row_add_account);

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

        // Fetch live/cached EDS mail sources
        let eds_sources = EdsSourceCache::new().load(EdsSourceKind::Mail);
        if eds_sources.is_empty() {
            let no_accounts = adw::ActionRow::builder()
                .title("Keine aktiven Konten gefunden")
                .subtitle("Verwenden Sie „Neues Konto hinzufügen…“, um ein Konto einzurichten")
                .build();
            let icon_no_accounts = gtk::Image::from_icon_name("dialog-information-symbolic");
            no_accounts.add_prefix(&icon_no_accounts);
            group_accounts.add(&no_accounts);
        } else {
            for source in eds_sources {
                let backend = if source.backend_name.trim().is_empty() {
                    "unbekannter Backendtyp"
                } else {
                    source.backend_name.trim()
                };
                let state = if source.enabled {
                    "aktiv"
                } else {
                    "deaktiviert"
                };
                let row = adw::ActionRow::builder()
                    .title(&source.display_name)
                    .subtitle(format!(
                        "{backend} · {state} · EDS-Konto (Klicken für benutzerdefinierte Einstellungen)"
                    ))
                    .activatable(true)
                    .build();
                let icon_mail = gtk::Image::from_icon_name("mail-read-symbolic");
                row.add_prefix(&icon_mail);

                let source_display_name = source.display_name.clone();
                let source_uid = source.uid.clone();
                let source_backend = source.backend_name.clone();
                let page_weak = self.downgrade();
                let open_eds_editor = move || {
                    let store = AccountConfigStore::default();
                    let profiles = store.load();
                    let matched_profile = profiles.into_iter().find(|p| {
                        p.id == format!("eds:{}", source_uid)
                            || p.display_name == source_display_name
                            || (!p.email.is_empty() && source_display_name.contains(&p.email))
                    });
                    let profile_to_edit = matched_profile.unwrap_or_else(|| {
                        let is_pop = source_backend.to_ascii_lowercase().contains("pop");
                        let is_exchange = source_backend.to_ascii_lowercase().contains("ews")
                            || source_backend.to_ascii_lowercase().contains("exchange");
                        let is_goa = source_backend.to_ascii_lowercase().contains("goa");
                        let protocol = if is_pop {
                            AccountProtocol::Pop3
                        } else if is_exchange {
                            AccountProtocol::Exchange
                        } else if is_goa {
                            AccountProtocol::Goa
                        } else {
                            AccountProtocol::ImapSmtp
                        };
                        let email = if source_display_name.contains('@') {
                            source_display_name.clone()
                        } else {
                            String::new()
                        };
                        AccountProfile {
                            id: format!("eds:{}", source_uid),
                            display_name: source_display_name.clone(),
                            email,
                            protocol,
                            ..Default::default()
                        }
                    });
                    if let Some(page) = page_weak.upgrade() {
                        page.present_account_dialog(crate::dialogs::AccountSetupDialog::new_for_edit(
                            &profile_to_edit,
                        ));
                    }
                };
                let open_for_row = open_eds_editor.clone();
                row.connect_activated(move |_| {
                    open_for_row();
                });
                let edit_btn = gtk::Button::builder()
                    .icon_name("document-edit-symbolic")
                    .tooltip_text("Benutzerdefinierte Einstellungen für dieses EDS-Konto anpassen")
                    .css_classes(vec!["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                edit_btn.connect_clicked(move |_| {
                    open_eds_editor();
                });
                row.add_suffix(&edit_btn);

                group_accounts.add(&row);
            }
        }

        page_accounts.add(&group_accounts);

        let group_profiles = adw::PreferencesGroup::builder()
            .title("Lokale Kontoprofile")
            .description(
                "Nicht-geheime Profile für die spätere Kontosynchronisation; Passwörter werden nicht gespeichert",
            )
            .build();
        self.imp()
            .local_profile_group
            .replace(Some(group_profiles.clone()));
        self.refresh_local_profile_rows();
        page_accounts.add(&group_profiles);

        // Filter Rules Section
        let group_filters = adw::PreferencesGroup::builder()
            .title("Mail-Filter &amp; Sortierung")
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

        let row_test_all = adw::ActionRow::builder()
            .title("Testlauf für alle aktiven Filterregeln…")
            .subtitle("Simulation auf dem lokalen Mailbestand ohne Änderungen anzuwenden")
            .activatable(true)
            .build();
        let icon_test = gtk::Image::from_icon_name("system-search-symbolic");
        row_test_all.add_prefix(&icon_test);
        let dialog_weak_for_all_sim = self.downgrade();
        let settings_for_all_sim = settings.clone();
        row_test_all.connect_activated(move |_| {
            let Some(dialog) = dialog_weak_for_all_sim.upgrade() else {
                return;
            };
            let (sender, receiver) = async_channel::bounded(1);
            let local_mail_directory = settings_for_all_sim
                .as_ref()
                .map(|settings| settings.local_mail_directory());
            let worker_result = std::thread::Builder::new()
                .name("simulate-all-filters".to_string())
                .spawn(move || {
                    let store = match local_mail_directory.as_deref().filter(|path| !path.trim().is_empty()) {
                        Some(dir) => MailStoreService::with_mail_dir(PathBuf::from(dir)),
                        None => MailStoreService::new(),
                    };
                    let report = store.simulate_all_filters();
                    let _ = sender.send_blocking(report);
                });
            if let Err(error) = worker_result {
                dialog.show_toast(&format!(
                    "Testlauf fehlgeschlagen: Worker konnte nicht gestartet werden: {error}"
                ));
                return;
            }
            let dialog_weak = dialog.downgrade();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await;
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(report) => {
                        dialog.show_toast(&format!(
                            "Testlauf für alle Filter: {} Treffer bei {} geprüften Nachricht(en)",
                            report.matches.len(),
                            report.total_messages_evaluated,
                        ));
                    }
                    Err(_) => {}
                }
            });
        });
        group_filters.add(&row_test_all);

        let row_log_actions = adw::SwitchRow::builder()
            .title("Filteraktionen protokollieren")
            .subtitle("Schreibt ausgeführte Filteraktionen und Fehler in die Filter-Protokolldatei")
            .build();
        if let Some(settings) = settings.clone() {
            row_log_actions.set_active(settings.filters_log_actions());
            let settings_for_log = settings.clone();
            row_log_actions.connect_active_notify(move |switch| {
                settings_for_log.set_filters_log_actions(switch.is_active());
            });
        }
        group_filters.add(&row_log_actions);

        let row_log_file = adw::EntryRow::builder()
            .title("Filter-Protokolldatei")
            .show_apply_button(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_log_file.set_text(&settings.filters_log_file());
            let settings_for_file = settings.clone();
            row_log_file.connect_apply(move |entry| {
                settings_for_file.set_filters_log_file(entry.text().as_str());
            });
        }
        group_filters.add(&row_log_file);
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
            let rule_for_run = rule.clone();
            let dialog_weak_for_run = self.downgrade();
            let run_button_for_result = run_button.clone();
            let settings_for_run = settings.clone();
            run_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_run.upgrade() else {
                    return;
                };
                run_button_for_result.set_sensitive(false);
                let (sender, receiver) = async_channel::bounded(1);
                let local_mail_directory = settings_for_run
                    .as_ref()
                    .map(|settings| settings.local_mail_directory());
                let snapshot = match ManualFilterRunSnapshot::capture(
                    rule_id_for_run.clone(),
                    rule_for_run.clone(),
                    MailStoreWorkerConfig::capture(local_mail_directory),
                    MailFilterStore::from_environment(),
                ) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        run_button_for_result.set_sensitive(true);
                        dialog.show_toast(&error.toast_message());
                        return;
                    }
                };
                let snapshot_path = snapshot.filter_store.path().to_path_buf();
                let completion_allowed = Rc::new(Cell::new(true));
                let completion_allowed_for_close = Rc::clone(&completion_allowed);
                dialog.connect_closed(move |_| {
                    completion_allowed_for_close.set(false);
                });
                let worker_result = std::thread::Builder::new()
                    .name("manual-mail-filter".to_string())
                    .spawn(move || {
                        let result = snapshot.run_on_worker();
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Manual filter result was dropped");
                        }
                    });
                if let Err(error) = worker_result {
                    cleanup_manual_filter_snapshot(&snapshot_path);
                    run_button_for_result.set_sensitive(true);
                    dialog.show_toast(&format!(
                        "Lokaler Filterlauf fehlgeschlagen: Worker konnte nicht gestartet werden: {error}"
                    ));
                    return;
                }
                let dialog_weak = dialog.downgrade();
                let run_button = run_button_for_result.clone();
                glib::spawn_future_local(async move {
                    let result = receiver.recv().await;
                    if !completion_allowed.get() {
                        return;
                    }
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    run_button.set_sensitive(true);
                    if matches!(&result, Ok(Ok(Some(_)))) {
                        if let Some(window) = dialog
                            .root()
                            .and_downcast::<crate::window::EvolutionWindow>()
                        {
                            window.refresh_mail_page();
                        }
                    }
                    dialog.show_toast(&manual_filter_result_toast(&result));
                });
            });
            row.add_suffix(&run_button);

            let simulate_button = gtk::Button::builder()
                .icon_name("system-search-symbolic")
                .tooltip_text("Testlauf für diese Regel ausführen (ohne Änderungen anzuwenden)")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let rule_id_for_sim = rule.id.clone();
            let dialog_weak_for_sim = self.downgrade();
            let simulate_button_for_result = simulate_button.clone();
            let settings_for_sim = settings.clone();
            simulate_button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak_for_sim.upgrade() else {
                    return;
                };
                simulate_button_for_result.set_sensitive(false);
                let (sender, receiver) = async_channel::bounded(1);
                let local_mail_directory = settings_for_sim
                    .as_ref()
                    .map(|settings| settings.local_mail_directory());
                let rule_id = rule_id_for_sim.clone();
                let worker_result = std::thread::Builder::new()
                    .name("simulate-mail-filter".to_string())
                    .spawn(move || {
                        let store = match local_mail_directory.as_deref().filter(|path| !path.trim().is_empty()) {
                            Some(dir) => MailStoreService::with_mail_dir(PathBuf::from(dir)),
                            None => MailStoreService::new(),
                        };
                        let report = store.simulate_filter_rule(&rule_id);
                        let _ = sender.send_blocking(report);
                    });
                if let Err(error) = worker_result {
                    simulate_button_for_result.set_sensitive(true);
                    dialog.show_toast(&format!(
                        "Testlauf fehlgeschlagen: Worker konnte nicht gestartet werden: {error}"
                    ));
                    return;
                }
                let dialog_weak = dialog.downgrade();
                let simulate_button = simulate_button_for_result.clone();
                glib::spawn_future_local(async move {
                    let result = receiver.recv().await;
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    simulate_button.set_sensitive(true);
                    match result {
                        Ok(Ok(Some(report))) => {
                            dialog.show_toast(&format!(
                                "Testlauf abgeschlossen: {} von {} Nachricht(en) treffen zu (ohne Änderungen)",
                                report.matches.len(),
                                report.total_messages_evaluated,
                            ));
                        }
                        Ok(Ok(None)) => {
                            dialog.show_toast("Filterregel wurde für den Testlauf nicht gefunden");
                        }
                        Ok(Err(error)) => {
                            dialog.show_toast(&format!("Testlauf fehlgeschlagen: {error}"));
                        }
                        Err(_) => {}
                    }
                });
            });
            row.add_suffix(&simulate_button);

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

        let group_sync = adw::PreferencesGroup::builder()
            .title("Synchronisation")
            .description("Automatische Aktualisierung der Mailordner")
            .build();
        let row_offline_mode = adw::SwitchRow::builder()
            .title("Offline-Modus")
            .subtitle(
                "Verwendet ausschließlich den lokalen Bestand und pausiert die Kontosynchronisation",
            )
            .build();
        if let Some(settings) = settings.clone() {
            row_offline_mode.set_active(settings.offline_mode());
            let dialog_for_offline = self.downgrade();
            row_offline_mode.connect_active_notify(move |row| {
                settings.set_offline_mode(row.is_active());
                if let Some(dialog) = dialog_for_offline.upgrade() {
                    if let Some(window) = dialog
                        .root()
                        .and_downcast::<crate::window::EvolutionWindow>()
                    {
                        window.refresh_mail_page();
                    }
                }
            });
        }
        group_sync.add(&row_offline_mode);

        let row_flush_outbox = adw::SwitchRow::builder()
            .title("Postausgang beim Abrufen leeren")
            .subtitle("Sendet wartende Nachrichten im Postausgang automatisch ab, wenn neue E-Mails abgerufen werden")
            .build();
        if let Some(settings) = settings.clone() {
            row_flush_outbox.set_active(settings.flush_outbox());
            let settings_for_flush = settings.clone();
            row_flush_outbox.connect_active_notify(move |switch| {
                settings_for_flush.set_flush_outbox(switch.is_active());
            });
        }
        group_sync.add(&row_flush_outbox);

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

        let row_sync_message_limit = adw::SpinRow::with_range(25.0, 1_000.0, 25.0);
        row_sync_message_limit.set_title("Nachrichten pro Ordner laden");
        row_sync_message_limit.set_subtitle(
            "Nachrichten je Abrufseite; weitere Nachrichten können mit „Mehr laden“ abgerufen werden",
        );
        row_sync_message_limit.set_digits(0);
        row_sync_message_limit.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_sync_message_limit.set_value(settings.mail_sync_message_limit() as f64);
            row_sync_message_limit.connect_value_notify(move |row| {
                settings.set_mail_sync_message_limit(row.value().round() as u32);
            });
        }
        group_sync.add(&row_sync_message_limit);

        let row_draft_autosave = adw::SpinRow::with_range(0.0, 3_600.0, 5.0);
        row_draft_autosave.set_title("Entwürfe automatisch speichern");
        row_draft_autosave.set_subtitle("Sekunden; 0 deaktiviert das lokale Autosave");
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
        page_network.add(&group_sync);

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
                        let destination_service = crate::services::CalendarStoreService::new();
                        if !destination_service.is_writable() {
                            if let Some(dialog) = dialog_for_file.upgrade() {
                                dialog.show_toast(
                                    "Der konfigurierte Kalenderordner ist extern und read-only; bitte zuerst einen app-eigenen Ordner wählen",
                                );
                            }
                            return;
                        }
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = destination_service.import_files(&paths);
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
                    let destination_service = crate::services::ContactStoreService::new();
                    if !destination_service.is_writable() {
                        if let Some(dialog) = dialog_for_file.upgrade() {
                            dialog.show_toast(
                                "Der konfigurierte Kontaktordner ist extern und read-only; bitte zuerst einen app-eigenen Ordner wählen",
                            );
                        }
                        return;
                    }
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = destination_service.import_files(&paths);
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
        page_contacts.add(&group_local_sources);

        let group_default_address_book = adw::PreferencesGroup::builder()
            .title("Standardadressbuch")
            .description("Neue Kontakte und die lokale Kontaktsuche verwenden diese Quelle")
            .build();
        group_default_address_book.add(&source_preference_row(
            "Standardadressbuch",
            "Synchronisierte und lokale Adressbücher",
            EdsSourceKind::AddressBook,
            "local-addressbook",
            "Lokales Adressbuch",
            settings.clone(),
            AppSettings::default_address_book_source_uid,
            AppSettings::set_default_address_book_source_uid,
        ));
        page_contacts.add(&group_default_address_book);

        let group_contacts_display = adw::PreferencesGroup::builder()
            .title("Darstellung &amp; Karten")
            .description("Adressformatierung, Anzeigereihenfolge und Kartendienste")
            .build();

        let row_address_formatting = adw::SwitchRow::builder()
            .title("Adressformatierung")
            .subtitle("Adressen in der Kontaktansicht mit Zeilenumbrüchen strukturieren")
            .build();

        let row_home_before_work = adw::SwitchRow::builder()
            .title("Privatadresse vor Arbeitsadresse")
            .subtitle("Private Kontaktdaten in der Vorschau vor geschäftlichen priorisieren")
            .build();

        let row_preview_show_maps = adw::SwitchRow::builder()
            .title("Kartenhinweise anzeigen")
            .subtitle("Kartenverfügbarkeit in der Kontaktvorschau signalisieren")
            .build();

        let row_open_map_target = adw::ComboRow::builder()
            .title("Kartendienst")
            .subtitle("Dienst zum Öffnen von Adressen auf der Karte")
            .model(&gtk::StringList::new(&["OpenStreetMap", "Google Maps"]))
            .build();

        let row_open_map_prefer_local = adw::SwitchRow::builder()
            .title("Lokale Kartenanwendung bevorzugen")
            .subtitle("Systemeigene geo:-Karten-Apps vor dem Webbrowser bevorzugen")
            .build();

        let row_contact_preview_visible = adw::SwitchRow::builder()
            .title("Kontaktvorschau einblenden")
            .subtitle("Vorschaubereich für ausgewählte Kontakte anzeigen")
            .build();

        if let Some(settings_ref) = settings.clone() {
            row_address_formatting.set_active(settings_ref.contact_address_formatting());
            let s_formatting = settings_ref.clone();
            row_address_formatting.connect_active_notify(move |row| {
                s_formatting.set_contact_address_formatting(row.is_active());
            });

            row_home_before_work.set_active(settings_ref.contact_preview_home_before_work());
            let s_home = settings_ref.clone();
            row_home_before_work.connect_active_notify(move |row| {
                s_home.set_contact_preview_home_before_work(row.is_active());
            });

            row_preview_show_maps.set_active(settings_ref.contact_preview_show_maps());
            let s_maps = settings_ref.clone();
            row_preview_show_maps.connect_active_notify(move |row| {
                s_maps.set_contact_preview_show_maps(row.is_active());
            });

            let map_target_idx = if settings_ref
                .contact_open_map_target()
                .eq_ignore_ascii_case("google")
            {
                1
            } else {
                0
            };
            row_open_map_target.set_selected(map_target_idx);
            let s_target = settings_ref.clone();
            row_open_map_target.connect_selected_notify(move |row| {
                let target = if row.selected() == 1 {
                    "google"
                } else {
                    "openstreetmap"
                };
                s_target.set_contact_open_map_target(target);
            });

            row_open_map_prefer_local.set_active(settings_ref.contact_open_map_prefer_local());
            let s_prefer = settings_ref.clone();
            row_open_map_prefer_local.connect_active_notify(move |row| {
                s_prefer.set_contact_open_map_prefer_local(row.is_active());
            });

            row_contact_preview_visible.set_active(settings_ref.contact_preview_visible());
            let s_preview = settings_ref.clone();
            row_contact_preview_visible.connect_active_notify(move |row| {
                s_preview.set_contact_preview_visible(row.is_active());
            });
        }

        group_contacts_display.add(&row_address_formatting);
        group_contacts_display.add(&row_home_before_work);
        group_contacts_display.add(&row_preview_show_maps);
        group_contacts_display.add(&row_open_map_target);
        group_contacts_display.add(&row_open_map_prefer_local);
        group_contacts_display.add(&row_contact_preview_visible);
        page_contacts.add(&group_contacts_display);

        let group_contacts_autocomplete = adw::PreferencesGroup::builder()
            .title("Autovervollständigung")
            .description("Einstellungen für die Adress- und Kontaktsuche beim Verfassen")
            .build();

        let row_completion_show_address = adw::SwitchRow::builder()
            .title("E-Mail-Adresse anzeigen")
            .subtitle("E-Mail-Adresse neben dem Namen in Vorschlägen einblenden")
            .build();

        let row_completion_min_query_length = adw::SpinRow::with_range(1.0, 100.0, 1.0);
        row_completion_min_query_length.set_title("Minimale Suchlänge");
        row_completion_min_query_length
            .set_subtitle("Mindestanzahl an Zeichen vor Beginn der Adresssuche");
        row_completion_min_query_length.set_digits(0);
        row_completion_min_query_length.set_snap_to_ticks(true);

        if let Some(settings_ref) = settings.clone() {
            row_completion_show_address.set_active(settings_ref.contact_completion_show_address());
            let s_comp_addr = settings_ref.clone();
            row_completion_show_address.connect_active_notify(move |row| {
                s_comp_addr.set_contact_completion_show_address(row.is_active());
            });

            row_completion_min_query_length
                .set_value(settings_ref.contact_completion_minimum_query_length() as f64);
            let s_comp_len = settings_ref.clone();
            row_completion_min_query_length.connect_value_notify(move |row| {
                s_comp_len.set_contact_completion_minimum_query_length(row.value().round() as u32);
            });
        }

        group_contacts_autocomplete.add(&row_completion_show_address);
        group_contacts_autocomplete.add(&row_completion_min_query_length);
        page_contacts.add(&group_contacts_autocomplete);

        let group_contacts_editor = adw::PreferencesGroup::builder()
            .title("Kontakteditor-Abschnitte")
            .description("Sichtbarkeit der einzelnen Detailabschnitte im Bearbeitungsdialog")
            .build();

        fn add_contact_editor_toggle(
            group: &adw::PreferencesGroup,
            settings: &Option<AppSettings>,
            title: &str,
            subtitle: &str,
            getter: fn(&AppSettings) -> bool,
            setter: fn(&AppSettings, bool),
        ) {
            let row = adw::SwitchRow::builder()
                .title(title)
                .subtitle(subtitle)
                .build();
            if let Some(s) = settings.clone() {
                row.set_active(getter(&s));
                let s_clone = s.clone();
                row.connect_active_notify(move |r| {
                    setter(&s_clone, r.is_active());
                });
            }
            group.add(&row);
        }

        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Telefonnummern",
            "Telefon- und Mobilnummern",
            AppSettings::contact_editor_show_phone,
            AppSettings::set_contact_editor_show_phone,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "SIP- / VoIP-Adressen",
            "Internettelefonie-Konten",
            AppSettings::contact_editor_show_sip,
            AppSettings::set_contact_editor_show_sip,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Instant Messaging",
            "Chat- und Messenger-Kennungen",
            AppSettings::contact_editor_show_im,
            AppSettings::set_contact_editor_show_im,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Privatadresse",
            "Wohnsitz und private Anschrift",
            AppSettings::contact_editor_show_home_address,
            AppSettings::set_contact_editor_show_home_address,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Geschäftsadresse",
            "Büro- und Firmenanschrift",
            AppSettings::contact_editor_show_work_address,
            AppSettings::set_contact_editor_show_work_address,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Weitere Adressen",
            "Sonstige postalische Adressen",
            AppSettings::contact_editor_show_other_address,
            AppSettings::set_contact_editor_show_other_address,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Websites",
            "Homepages und Weblinks",
            AppSettings::contact_editor_show_web,
            AppSettings::set_contact_editor_show_web,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Beruf &amp; Organisation",
            "Position, Abteilung und Firma",
            AppSettings::contact_editor_show_job,
            AppSettings::set_contact_editor_show_job,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Geburts- und Jahrestage",
            "Persönliche Termine und Jubiläen",
            AppSettings::contact_editor_show_dates,
            AppSettings::set_contact_editor_show_dates,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Zusatzangaben",
            "Zeitzone, Geokoordinaten und Rollen",
            AppSettings::contact_editor_show_misc,
            AppSettings::set_contact_editor_show_misc,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Notizen",
            "Freitext-Notizen zum Kontakt",
            AppSettings::contact_editor_show_notes,
            AppSettings::set_contact_editor_show_notes,
        );
        add_contact_editor_toggle(
            &group_contacts_editor,
            &settings,
            "Zertifikate",
            "S/MIME- und Verschlüsselungszertifikate",
            AppSettings::contact_editor_show_certs,
            AppSettings::set_contact_editor_show_certs,
        );
        page_contacts.add(&group_contacts_editor);

        let group_backup = adw::PreferencesGroup::builder()
            .title("Daten &amp; Sicherung")
            .description(
                "App-eigene Zustände sichern, diagnostizieren, migrieren oder löschen; externe Profile bleiben unberührt",
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
                        let inspection = match backup_service::inspect_backup(&bytes) {
                            Ok(inspection) => inspection,
                            Err(error) => {
                                if let Some(dialog) = dialog_for_file.upgrade() {
                                    dialog.show_toast(&format!(
                                        "Sicherung ist ungültig oder beschädigt: {error}"
                                    ));
                                }
                                return;
                            }
                        };
                        let Some(dialog) = dialog_for_file.upgrade() else {
                            return;
                        };
                        let checksum_status = if inspection.legacy_files_without_checksum == 0 {
                            "alle Dateien geprüft"
                        } else {
                            "ältere Dateien ohne Prüfsumme enthalten"
                        };
                        let settings_status = if inspection.settings_present {
                            "Einstellungen enthalten"
                        } else {
                            "keine Einstellungen enthalten"
                        };
                        let confirmation = adw::AlertDialog::new(
                            Some("App-Daten wiederherstellen?"),
                            Some(&format!(
                                "{} Datei(en), {}; {}; vorhandene app-eigene Dateien aus dieser Sicherung werden ersetzt. Externe Profile bleiben unverändert.",
                                inspection.files,
                                settings_status,
                                checksum_status
                            )),
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
                    Some("Kontenprofile, Entwürfe, lokale ICS-/vCard-Daten und app-eigene Einstellungen werden entfernt. Externe Profile, sichere Zugangsdaten und ausgewählte externe Ordner bleiben unverändert."),
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
            .subtitle("Nicht-geheime Kontenmetadaten sowie valide lokale iCalendar-/vCard-Dateien aus einer ausgewählten Profilkopie übernehmen")
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
                        let calendar_store =
                            crate::services::CalendarStoreService::with_writable_calendar_dir(
                                crate::services::app_data_dir().join("ical"),
                            );
                        let contact_store =
                            crate::services::ContactStoreService::with_writable_contact_dir(
                                crate::services::app_data_dir().join("vcards"),
                            );
                        let mail_directory = crate::services::app_data_dir().join("mail");
                        let mail_state_file =
                            crate::services::app_data_dir().join("mail-state.json");
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_local_mail_directory(&mail_directory.to_string_lossy());
                        }
                        let mail_store =
                            crate::services::MailStoreService::with_mail_dir_and_state_path(
                                mail_directory,
                                mail_state_file,
                            );
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = migration_service::import_evolution_profile_with_cache_and_local_data(
                                path,
                                &AccountConfigStore::default(),
                                &source_cache,
                                &calendar_store,
                                &contact_store,
                                &mail_store,
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
                                    let imported_setting_keys = report
                                        .settings
                                        .iter()
                                        .map(|setting| setting.key.as_str())
                                        .collect::<std::collections::HashSet<_>>();
                                    if let Some(settings) = AppSettings::try_new() {
                                        for setting in &report.settings {
                                            if let Err(error) = settings
                                                .apply_migrated_setting(&setting.key, &setting.value)
                                            {
                                                tracing::warn!(
                                                    key = %setting.key,
                                                    %error,
                                                    "Evolution-Einstellung konnte nicht übernommen werden"
                                                );
                                            }
                                        }
                                        // Local profile files were copied into app-owned stores;
                                        // never point the running application back at the original
                                        // Evolution profile after migration.
                                        settings.set_local_mail_directory(
                                            &crate::services::app_data_dir()
                                                .join("mail")
                                                .to_string_lossy(),
                                        );
                                        settings.set_local_calendar_directory(
                                            &crate::services::app_data_dir()
                                                .join("ical")
                                                .to_string_lossy(),
                                        );
                                        settings.set_local_contact_directory(
                                            &crate::services::app_data_dir()
                                                .join("vcards")
                                                .to_string_lossy(),
                                        );
                                        if !imported_setting_keys.contains("signature-text") {
                                            if let Some(signature) = report.signature_text.as_deref() {
                                                settings.set_signature_text(signature);
                                                if !imported_setting_keys.contains("append-signature") {
                                                    settings.set_append_signature(true);
                                                }
                                            }
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
                                        "{} Profilquelle(n) gefunden: {} Konto/Konten, {} Mail-, {} Kalender- und {} Adressbuchquelle(n) zwischengespeichert, {} Filterregel(n), {} Signatur(en), {} Einstellungswert(e) aus {} Datei(en), {} Maildatei(en), {} lokale Mailordner, {} iCalendar-Datei(en) und {} vCard-Datei(en) übernommen, {} übersprungen{}{}",
                                        report.discovered_sources,
                                        report.imported_accounts,
                                        report.cached_mail_sources,
                                        report.cached_calendar_sources,
                                        report.cached_address_book_sources,
                                        report.imported_filters,
                                        report.imported_signatures,
                                        report.imported_settings,
                                        report.discovered_settings_files,
                                        report.imported_mail_files,
                                        report.imported_mail_folders,
                                        report.imported_calendar_files,
                                        report.imported_contact_files,
                                        report.skipped_sources,
                                        warning_suffix,
                                        secret_suffix
                                    ));
                                    if let Some(window) = dialog
                                        .root()
                                        .and_downcast::<crate::window::EvolutionWindow>()
                                    {
                                        window.refresh_mail_page();
                                    }
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
                    let source_store = crate::services::MailStoreService::new();
                    let destination = source_store.import_destination();
                    let state_path = crate::services::app_data_dir().join("mail-state.json");
                    if let Some(settings) = AppSettings::try_new() {
                        settings.set_local_mail_directory(&destination.to_string_lossy());
                    }
                    let destination_store =
                        crate::services::MailStoreService::with_mail_dir_and_state_path(
                            destination,
                            state_path,
                        );
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = destination_store.import_files(&paths);
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
                        let source_store = crate::services::MailStoreService::new();
                        let destination = source_store.import_destination();
                        let state_path = crate::services::app_data_dir().join("mail-state.json");
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_local_mail_directory(&destination.to_string_lossy());
                        }
                        let destination_store =
                            crate::services::MailStoreService::with_mail_dir_and_state_path(
                                destination,
                                state_path,
                            );
                        let (sender, receiver) = async_channel::bounded(1);
                        std::thread::spawn(move || {
                            let result = destination_store
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
        page_backup.add(&group_backup);

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

        let row_request_read_receipt = adw::SwitchRow::builder()
            .title("Lesebestätigung anfordern")
            .subtitle(
                "Fügt neuen Nachrichten den RFC-konformen Wunsch nach einer Lesebestätigung hinzu",
            )
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_request_read_receipt.set_active(settings.request_read_receipt());
            row_request_read_receipt.connect_active_notify(move |row| {
                settings.set_request_read_receipt(row.is_active());
            });
        }
        group_composer.add(&row_request_read_receipt);

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

        let group_signature_editor = adw::PreferencesGroup::builder()
            .title("Signaturtext &amp; Inhalt")
            .description("Der Inhalt der Signatur, der beim Erstellen neuer E-Mails automatisch eingefügt wird")
            .build();

        let signature_frame = gtk::Frame::builder()
            .css_classes(vec!["card"])
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let signature_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(120)
            .max_content_height(220)
            .propagate_natural_height(true)
            .child(&signature_view)
            .build();

        signature_frame.set_child(Some(&signature_scroll));
        group_signature_editor.add(&signature_frame);

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

        // ── 2. Page: Erscheinungsbild & Leseansicht ─────────────────────────
        let page_appearance = adw::PreferencesPage::builder()
            .title("Erscheinungsbild")
            .icon_name("display-brightness-symbolic")
            .build();

        let group_navigation = adw::PreferencesGroup::builder()
            .title("Navigation &amp; Modul-Schaltflächen")
            .description(
                "Positionierung der Schaltflächen für Mail, Kalender, Kontakte, Aufgaben und Memos",
            )
            .build();
        let row_switcher_location = adw::ComboRow::builder()
            .title("Position der Modul-Schaltflächen")
            .model(&gtk::StringList::new(&[
                "Oben in der Kopfleiste (Libadwaita)",
                "Unten links in der Seitenleiste (Klassisches Evolution)",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_switcher_location.set_selected(if settings.switcher_location() == "bottom-left" {
                1
            } else {
                0
            });
            row_switcher_location.connect_selected_notify(move |row| {
                settings.set_switcher_location(if row.selected() == 1 {
                    "bottom-left"
                } else {
                    "top"
                });
            });
        }
        group_navigation.add(&row_switcher_location);
        page_appearance.add(&group_navigation);

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
            .subtitle("Deaktivieren für reine Textanzeige")
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
        row_mail_font_size.set_subtitle("8–48 Pixel; Standard 16 Pixel");
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

        let timeout_options = gtk::DropDown::from_strings(&[
            "Sofort",
            "Nach 0,5 Sekunden",
            "Nach 1,5 Sekunden (Standard)",
            "Nach 3 Sekunden",
            "Nach 5 Sekunden",
        ]);
        timeout_options.set_valign(gtk::Align::Center);
        let row_mark_read_timeout = adw::ActionRow::builder()
            .title("Verzögerung vor Gelesen-Markierung")
            .subtitle("Verhindert vorzeitiges Markieren beim schnellen Durchblättern")
            .build();
        row_mark_read_timeout.add_suffix(&timeout_options);

        if let Some(settings) = settings.clone() {
            let timeout_ms = settings.mark_read_timeout();
            let selected_idx = match timeout_ms {
                t if t <= 0 => 0,
                t if t <= 800 => 1,
                t if t <= 2000 => 2,
                t if t <= 4000 => 3,
                _ => 4,
            };
            timeout_options.set_selected(selected_idx);
            timeout_options.connect_selected_notify(move |dropdown| {
                let ms = match dropdown.selected() {
                    0 => 0,
                    1 => 500,
                    2 => 1500,
                    3 => 3000,
                    _ => 5000,
                };
                settings.set_mark_read_timeout(ms);
            });
        }
        group_reading.add(&row_mark_read_timeout);

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

        let row_mail_security_details = adw::SwitchRow::builder()
            .title("Sicherheitsstatus für normale E-Mails anzeigen")
            .subtitle(
                "Zeigt OpenPGP-/S/MIME-Status und Details; Warnungen sowie gültige oder fehlgeschlagene Prüfungen bleiben sichtbar",
            )
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_security_details.set_active(settings.mail_show_security_details());
            row_mail_security_details.connect_active_notify(move |row| {
                settings.set_mail_show_security_details(row.is_active());
            });
        }
        group_reading.add(&row_mail_security_details);

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

        let row_mail_toolbar = adw::SwitchRow::builder()
            .title("Klassische Mail-Werkzeugleiste anzeigen")
            .subtitle(
                "Zeigt die vertrauten Evolution-Aktionen für neue Mail, Antworten, Archivieren und Löschen über dem gesamten Mailbereich",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_toolbar.set_active(settings.mail_show_toolbar());
            row_mail_toolbar.connect_active_notify(move |row| {
                settings.set_mail_show_toolbar(row.is_active());
            });
        }
        group_reading.add(&row_mail_toolbar);

        let row_mail_statusbar = adw::SwitchRow::builder()
            .title("Statusinformationen unten anzeigen")
            .subtitle(
                "Zeigt Konto-, Lade- und Verbindungsstatus ausschließlich am unteren Fensterrand",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_statusbar.set_active(settings.mail_show_statusbar());
            row_mail_statusbar.connect_active_notify(move |row| {
                settings.set_mail_show_statusbar(row.is_active());
            });
        }
        group_reading.add(&row_mail_statusbar);

        let row_mail_sidebar = adw::SwitchRow::builder()
            .title("Ordner-Seitenleiste anzeigen")
            .subtitle(
                "Behält die klassische Evolution-Ordneransicht links neben Nachrichtenliste und Vorschau sichtbar",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sidebar.set_active(settings.mail_show_sidebar());
            row_mail_sidebar.connect_active_notify(move |row| {
                settings.set_mail_show_sidebar(row.is_active());
            });
        }
        group_reading.add(&row_mail_sidebar);

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
            .active(false)
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
                "Nachverfolgung",
                "Wichtigkeit",
                "Anhänge",
                "Größe",
                "Fällig am",
                "Ort",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sort.set_selected(match settings.mail_sort_field().as_str() {
                "subject" => 1,
                "sender" => 2,
                "unread" => 3,
                "flagged" => 4,
                "follow-up" => 5,
                "important" => 6,
                "attachments" => 7,
                "size" => 8,
                "follow-up-due" => 9,
                "location" => 10,
                _ => 0,
            });
            row_mail_sort.connect_selected_notify(move |row| {
                settings.set_mail_sort_field(match row.selected() {
                    1 => "subject",
                    2 => "sender",
                    3 => "unread",
                    4 => "flagged",
                    5 => "follow-up",
                    6 => "important",
                    7 => "attachments",
                    8 => "size",
                    9 => "follow-up-due",
                    10 => "location",
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

        let group_mail_columns = adw::PreferencesGroup::builder()
            .title("Mail-Zeile")
            .description("Ansichtsprofil, Reihenfolge, sichtbare Spalten und Zusatzinformationen")
            .build();
        let row_mail_list_profile = adw::ComboRow::builder()
            .title("Ansichtsprofil")
            .subtitle("Standard entspricht Evolutions Messages-Ansicht; Wide priorisiert die Nachrichtenfelder")
            .model(&gtk::StringList::new(&["Standard", "Wide", "Benutzerdefiniert"]))
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_list_profile.set_selected(match settings.mail_list_view_profile().as_str() {
                "wide" => 1,
                "custom" => 2,
                _ => 0,
            });
            let settings_for_selection = settings.clone();
            row_mail_list_profile.connect_selected_notify(move |row| match row.selected() {
                0 => settings_for_selection.apply_mail_list_view_profile("messages"),
                1 => settings_for_selection.apply_mail_list_view_profile("wide"),
                _ => {
                    let order = settings_for_selection.mail_list_column_order();
                    settings_for_selection.set_mail_list_column_order(&order);
                }
            });
            let profile_row = row_mail_list_profile.downgrade();
            settings.connect_changed(Some("mail-list-view-profile"), move |settings, _| {
                let Some(row) = profile_row.upgrade() else {
                    return;
                };
                row.set_selected(match settings.string("mail-list-view-profile").as_str() {
                    "wide" => 1,
                    "custom" => 2,
                    _ => 0,
                });
            });
        }
        group_mail_columns.add(&row_mail_list_profile);
        group_mail_columns.add(&build_mail_column_order_editor(settings.clone()));

        let row_mail_show_avatar = adw::SwitchRow::builder()
            .title("Absender-Avatare anzeigen")
            .subtitle("Zeigt Initialen bzw. Bilder links neben dem Absender")
            .active(false)
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

        let row_mail_show_recipient = adw::SwitchRow::builder()
            .title("Empfänger anzeigen")
            .subtitle("Zeigt die An- bzw. Cc-Adresse unter dem Absender")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_recipient.set_active(settings.mail_show_recipient());
            row_mail_show_recipient.connect_active_notify(move |row| {
                settings.set_mail_show_recipient(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_recipient);

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

        let row_mail_show_attachment = adw::SwitchRow::builder()
            .title("Anhangssymbol anzeigen")
            .subtitle("Zeigt eine Büroklammer, wenn die Nachricht Anhänge enthält")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_attachment.set_active(settings.mail_show_attachment());
            row_mail_show_attachment.connect_active_notify(move |row| {
                settings.set_mail_show_attachment(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_attachment);

        let row_mail_show_preview_text = adw::SwitchRow::builder()
            .title("Vorschautext anzeigen")
            .subtitle("Zeigt den kurzen Nachrichtenauszug unter dem Betreff")
            .active(false)
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

        let row_mail_show_size = adw::SwitchRow::builder()
            .title("Größe anzeigen")
            .subtitle("Zeigt die vom Mailanbieter gemeldete Nachrichtengröße")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_size.set_active(settings.mail_show_size());
            row_mail_show_size.connect_active_notify(move |row| {
                settings.set_mail_show_size(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_size);

        let row_mail_show_follow_up_due = adw::SwitchRow::builder()
            .title("Fälligkeit anzeigen")
            .subtitle("Zeigt das Fälligkeitsdatum einer Nachverfolgung")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_follow_up_due.set_active(settings.mail_show_follow_up_due());
            row_mail_show_follow_up_due.connect_active_notify(move |row| {
                settings.set_mail_show_follow_up_due(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_follow_up_due);

        let row_mail_show_location = adw::SwitchRow::builder()
            .title("Ort anzeigen")
            .subtitle("Zeigt Konto und Ordner der Nachricht")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_location.set_active(settings.mail_show_location());
            row_mail_show_location.connect_active_notify(move |row| {
                settings.set_mail_show_location(row.is_active());
            });
        }
        group_mail_columns.add(&row_mail_show_location);

        let group_mail_normal_headers = adw::PreferencesGroup::builder()
            .title("Normale Nachrichtenkopfzeilen")
            .description(
                "Bestimmt die Reihenfolge und Sichtbarkeit der Kopfzeilen in der normalen Leseansicht. Deaktivierte Felder bleiben hier verfügbar.",
            )
            .build();
        let normal_headers_editor = build_mail_normal_headers_editor(settings.clone());
        group_mail_normal_headers.add(&normal_headers_editor);

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
                "Jahr",
                "Liste",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_calendar_view.set_selected(match settings.calendar_view().as_str() {
                "day" => 0,
                "week" => 1,
                "workweek" => 2,
                "month" => 3,
                "agenda" => 4,
                "year" => 5,
                "list" => 6,
                _ => 3,
            });
            row_calendar_view.connect_selected_notify(move |row| {
                settings.set_calendar_view(match row.selected() {
                    0 => "day",
                    1 => "week",
                    2 => "workweek",
                    4 => "agenda",
                    5 => "year",
                    6 => "list",
                    _ => "month",
                });
            });
        }
        group_calendar_view.add(&row_calendar_view);

        let row_time_divisions = adw::ComboRow::builder()
            .title("Zeiteinteilung")
            .subtitle("Intervall der Zeitleiste in Tages- und Arbeitswochenansicht")
            .model(&gtk::StringList::new(&[
                "60 Minuten",
                "30 Minuten",
                "15 Minuten",
                "10 Minuten",
                "5 Minuten",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_time_divisions.set_selected(match settings.calendar_time_divisions() {
                60 => 0,
                30 => 1,
                15 => 2,
                10 => 3,
                5 => 4,
                _ => 1,
            });
            row_time_divisions.connect_selected_notify(move |row| {
                settings.set_calendar_time_divisions(match row.selected() {
                    0 => 60,
                    1 => 30,
                    2 => 15,
                    3 => 10,
                    4 => 5,
                    _ => 30,
                });
            });
        }
        group_calendar_view.add(&row_time_divisions);

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

        let row_compress_weekend = adw::SwitchRow::builder()
            .title("Wochenende in Monatsansicht komprimieren")
            .subtitle("Samstag und Sonntag in einer gemeinsamen Spalte darstellen")
            .build();
        if let Some(settings) = settings.clone() {
            row_compress_weekend.set_active(settings.calendar_compress_weekend());
            row_compress_weekend.connect_active_notify(move |row| {
                settings.set_calendar_compress_weekend(row.is_active());
            });
        }
        group_calendar_view.add(&row_compress_weekend);

        let row_month_scroll_by_week = adw::SwitchRow::builder()
            .title("Monatsansicht wochenweise scrollen")
            .subtitle("Blättert beim Scrollen um eine Woche statt um einen ganzen Monat")
            .build();
        if let Some(settings) = settings.clone() {
            row_month_scroll_by_week.set_active(settings.calendar_month_scroll_by_week());
            row_month_scroll_by_week.connect_active_notify(move |row| {
                settings.set_calendar_month_scroll_by_week(row.is_active());
            });
        }
        group_calendar_view.add(&row_month_scroll_by_week);

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

        let row_calendar_time_zone = adw::EntryRow::builder().title("Kalender-Zeitzone").build();
        row_calendar_time_zone.set_tooltip_text(Some(
            "system für die Rechnerzeitzone, UTC oder eine IANA-Zone wie Europe/Berlin",
        ));
        if let Some(settings) = settings.clone() {
            row_calendar_time_zone.set_text(&settings.calendar_time_zone());
            row_calendar_time_zone.connect_changed(move |row| {
                settings.set_calendar_time_zone(&row.text());
            });
        }
        group_calendar_view.add(&row_calendar_time_zone);

        let row_second_zone = adw::EntryRow::builder().title("Zweite Zeitzone").build();
        row_second_zone.set_tooltip_text(Some(
            "Zusätzliche Zeitzone für die Tagesansicht (z. B. UTC oder America/New_York)",
        ));
        if let Some(settings) = settings.clone() {
            row_second_zone.set_text(&settings.calendar_second_zone());
            row_second_zone.connect_changed(move |row| {
                settings.set_calendar_second_zone(&row.text());
            });
        }
        group_calendar_view.add(&row_second_zone);

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

        let group_calendar_reminders = adw::PreferencesGroup::builder()
            .title("Erinnerungen &amp; Termine")
            .description("Standard-Erinnerungen für Termine und Benachrichtigungen")
            .build();

        let row_use_default_reminder = adw::SwitchRow::builder()
            .title("Standard-Erinnerung für Termine")
            .subtitle("Erinnerung automatisch zu neuen Terminen hinzufügen")
            .build();
        let row_default_reminder_interval = adw::SpinRow::with_range(1.0, 10_080.0, 1.0);
        row_default_reminder_interval.set_title("Standard-Erinnerungsintervall");
        row_default_reminder_interval.set_subtitle("Vor Beginn des Termins");
        row_default_reminder_interval.set_digits(0);
        row_default_reminder_interval.set_snap_to_ticks(true);

        let row_default_reminder_units = adw::ComboRow::builder()
            .title("Standard-Erinnerungseinheit")
            .model(&gtk::StringList::new(&["Minuten", "Stunden", "Tage"]))
            .build();

        if let Some(settings) = settings.clone() {
            let active = settings.use_default_reminder();
            row_use_default_reminder.set_active(active);
            row_default_reminder_interval.set_sensitive(active);
            row_default_reminder_units.set_sensitive(active);

            let row_interval_ref = row_default_reminder_interval.clone();
            let row_units_ref = row_default_reminder_units.clone();
            let settings_for_toggle = settings.clone();
            row_use_default_reminder.connect_active_notify(move |row| {
                let is_active = row.is_active();
                row_interval_ref.set_sensitive(is_active);
                row_units_ref.set_sensitive(is_active);
                settings_for_toggle.set_use_default_reminder(is_active);
            });

            row_default_reminder_interval.set_value(settings.default_reminder_interval() as f64);
            let settings_for_interval = settings.clone();
            row_default_reminder_interval.connect_value_notify(move |row| {
                settings_for_interval.set_default_reminder_interval(row.value().round() as u32);
            });

            row_default_reminder_units.set_selected(match settings.default_reminder_units() {
                ReminderDurationUnit::Minutes => 0,
                ReminderDurationUnit::Hours => 1,
                ReminderDurationUnit::Days => 2,
            });
            let settings_for_units = settings.clone();
            row_default_reminder_units.connect_selected_notify(move |row| {
                let unit = match row.selected() {
                    1 => ReminderDurationUnit::Hours,
                    2 => ReminderDurationUnit::Days,
                    _ => ReminderDurationUnit::Minutes,
                };
                settings_for_units.set_default_reminder_units(unit);
            });
        }
        group_calendar_reminders.add(&row_use_default_reminder);
        group_calendar_reminders.add(&row_default_reminder_interval);
        group_calendar_reminders.add(&row_default_reminder_units);

        let row_use_ba_reminder = adw::SwitchRow::builder()
            .title("Geburtstags- &amp; Jahrestagserinnerungen")
            .subtitle("Erinnerungen für Kontakte mit Geburts- oder Jahrestag")
            .build();
        let row_ba_reminder_interval = adw::SpinRow::with_range(1.0, 10_080.0, 1.0);
        row_ba_reminder_interval.set_title("Intervall für Geburtstage &amp; Jahrestage");
        row_ba_reminder_interval.set_digits(0);
        row_ba_reminder_interval.set_snap_to_ticks(true);

        let row_ba_reminder_units = adw::ComboRow::builder()
            .title("Einheit für Geburtstage &amp; Jahrestage")
            .model(&gtk::StringList::new(&["Minuten", "Stunden", "Tage"]))
            .build();

        if let Some(settings) = settings.clone() {
            let active = settings.use_ba_reminder();
            row_use_ba_reminder.set_active(active);
            row_ba_reminder_interval.set_sensitive(active);
            row_ba_reminder_units.set_sensitive(active);

            let row_interval_ref = row_ba_reminder_interval.clone();
            let row_units_ref = row_ba_reminder_units.clone();
            let settings_for_toggle = settings.clone();
            row_use_ba_reminder.connect_active_notify(move |row| {
                let is_active = row.is_active();
                row_interval_ref.set_sensitive(is_active);
                row_units_ref.set_sensitive(is_active);
                settings_for_toggle.set_use_ba_reminder(is_active);
            });

            row_ba_reminder_interval.set_value(settings.ba_reminder_interval() as f64);
            let settings_for_interval = settings.clone();
            row_ba_reminder_interval.connect_value_notify(move |row| {
                settings_for_interval.set_ba_reminder_interval(row.value().round() as u32);
            });

            row_ba_reminder_units.set_selected(match settings.ba_reminder_units() {
                ReminderDurationUnit::Minutes => 0,
                ReminderDurationUnit::Hours => 1,
                ReminderDurationUnit::Days => 2,
            });
            let settings_for_units = settings.clone();
            row_ba_reminder_units.connect_selected_notify(move |row| {
                let unit = match row.selected() {
                    1 => ReminderDurationUnit::Hours,
                    2 => ReminderDurationUnit::Days,
                    _ => ReminderDurationUnit::Minutes,
                };
                settings_for_units.set_ba_reminder_units(unit);
            });
        }
        group_calendar_reminders.add(&row_use_ba_reminder);
        group_calendar_reminders.add(&row_ba_reminder_interval);
        group_calendar_reminders.add(&row_ba_reminder_units);

        let row_hide_cancelled_events = adw::SwitchRow::builder()
            .title("Abgesagte Termine ausblenden")
            .subtitle("Termine mit Status CANCELLED nicht in Ansichten anzeigen")
            .build();
        if let Some(settings) = settings.clone() {
            row_hide_cancelled_events.set_active(settings.hide_cancelled_events());
            let settings_for_cancelled = settings.clone();
            row_hide_cancelled_events.connect_active_notify(move |row| {
                settings_for_cancelled.set_hide_cancelled_events(row.is_active());
            });
        }
        group_calendar_reminders.add(&row_hide_cancelled_events);

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

        let row_hide_completed_tasks = adw::SwitchRow::builder()
            .title("Erledigte Aufgaben ausblenden")
            .subtitle("Erledigte Aufgaben nach Ablauf einer Frist ausblenden")
            .build();
        let row_hide_completed_tasks_value = adw::SpinRow::with_range(0.0, 1000.0, 1.0);
        row_hide_completed_tasks_value.set_title("Frist für erledigte Aufgaben");
        row_hide_completed_tasks_value.set_subtitle("0 = sofort nach Erledigung ausblenden");
        row_hide_completed_tasks_value.set_digits(0);
        row_hide_completed_tasks_value.set_snap_to_ticks(true);

        let row_hide_completed_tasks_units = adw::ComboRow::builder()
            .title("Einheit für Erledigungsfrist")
            .model(&gtk::StringList::new(&["Minuten", "Stunden", "Tage"]))
            .build();

        if let Some(settings) = settings.clone() {
            let active = settings.hide_completed_tasks();
            row_hide_completed_tasks.set_active(active);
            row_hide_completed_tasks_value.set_sensitive(active);
            row_hide_completed_tasks_units.set_sensitive(active);

            let row_val_ref = row_hide_completed_tasks_value.clone();
            let row_units_ref = row_hide_completed_tasks_units.clone();
            let settings_for_toggle = settings.clone();
            row_hide_completed_tasks.connect_active_notify(move |row| {
                let is_active = row.is_active();
                row_val_ref.set_sensitive(is_active);
                row_units_ref.set_sensitive(is_active);
                settings_for_toggle.set_hide_completed_tasks(is_active);
            });

            row_hide_completed_tasks_value.set_value(settings.hide_completed_tasks_value() as f64);
            let settings_for_val = settings.clone();
            row_hide_completed_tasks_value.connect_value_notify(move |row| {
                settings_for_val.set_hide_completed_tasks_value(row.value().round() as u32);
            });

            row_hide_completed_tasks_units.set_selected(
                match settings.hide_completed_tasks_units() {
                    ReminderDurationUnit::Minutes => 0,
                    ReminderDurationUnit::Hours => 1,
                    ReminderDurationUnit::Days => 2,
                },
            );
            let settings_for_units = settings.clone();
            row_hide_completed_tasks_units.connect_selected_notify(move |row| {
                let unit = match row.selected() {
                    1 => ReminderDurationUnit::Hours,
                    2 => ReminderDurationUnit::Days,
                    _ => ReminderDurationUnit::Minutes,
                };
                settings_for_units.set_hide_completed_tasks_units(unit);
            });
        }
        group_task_memo_lists.add(&row_hide_completed_tasks);
        group_task_memo_lists.add(&row_hide_completed_tasks_value);
        group_task_memo_lists.add(&row_hide_completed_tasks_units);

        let row_hide_cancelled_tasks = adw::SwitchRow::builder()
            .title("Abgebrochene Aufgaben ausblenden")
            .subtitle("Aufgaben mit Status CANCELLED in Ansichten nicht anzeigen")
            .build();
        if let Some(settings) = settings.clone() {
            row_hide_cancelled_tasks.set_active(settings.hide_cancelled_tasks());
            let settings_for_cancelled = settings.clone();
            row_hide_cancelled_tasks.connect_active_notify(move |row| {
                settings_for_cancelled.set_hide_cancelled_tasks(row.is_active());
            });
        }
        group_task_memo_lists.add(&row_hide_cancelled_tasks);

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

        let group_crypto = adw::PreferencesGroup::builder()
            .title("Verschlüsselung &amp; Signaturen")
            .description("OpenPGP- und S/MIME-Konfiguration")
            .build();

        let crypto_protocol_model =
            gtk::StringList::new(&["OpenPGP (GnuPG)", "S/MIME (OpenSSL/CMS)"]);
        let row_crypto_protocol = adw::ComboRow::builder()
            .title("Standardprotokoll")
            .subtitle("Wird für neue Nachrichten und die Prüfung eingehender Nachrichten verwendet")
            .model(&crypto_protocol_model)
            .build();
        if let Some(settings) = settings.clone() {
            row_crypto_protocol.set_selected(u32::from(settings.crypto_protocol() == "smime"));
            row_crypto_protocol.connect_selected_notify(move |row| {
                settings.set_crypto_protocol(if row.selected() == 1 {
                    "smime"
                } else {
                    "openpgp"
                });
            });
        }
        group_crypto.add(&row_crypto_protocol);

        let row_pgp_encrypt = adw::SwitchRow::builder()
            .title("Neue Mails standardmäßig signieren")
            .subtitle("Verwendet das ausgewählte OpenPGP- oder S/MIME-Protokoll")
            .active(false)
            .build();
        group_crypto.add(&row_pgp_encrypt);

        let pgp_key_model =
            gtk::StringList::new(&["Automatisch anhand der Absenderadresse wählen"]);
        let pgp_keys = Rc::new(RefCell::new(Vec::<OpenPgpKey>::new()));
        let row_pgp_key = adw::ComboRow::builder()
            .title("Standard PGP-Schlüssel")
            .model(&pgp_key_model)
            .build();
        if let Some(settings) = settings.clone() {
            row_pgp_key.set_selected(0);
            let pgp_keys_for_selection = Rc::clone(&pgp_keys);
            row_pgp_key.connect_selected_notify(move |row| {
                if row.selected() == 0 {
                    settings.set_default_pgp_key("");
                } else if let Some(key) = pgp_keys_for_selection
                    .borrow()
                    .get(row.selected() as usize - 1)
                {
                    settings.set_default_pgp_key(&key.fingerprint);
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

        let row_smime_signer = smime_file_row(
            "S/MIME-Signaturzertifikat",
            "PEM- oder DER-Zertifikat für ausgehende Signaturen",
            settings
                .as_ref()
                .map(|settings| settings.smime_signer_certificate())
                .unwrap_or_default(),
            settings.clone(),
            AppSettings::set_smime_signer_certificate,
        );
        group_crypto.add(&row_smime_signer);

        let row_smime_private_key = smime_file_row(
            "S/MIME-Privatschlüssel",
            "Pfad zur privaten Schlüsseldatei; der Schlüssel selbst wird nicht kopiert",
            settings
                .as_ref()
                .map(|settings| settings.smime_private_key())
                .unwrap_or_default(),
            settings.clone(),
            AppSettings::set_smime_private_key,
        );
        group_crypto.add(&row_smime_private_key);

        let row_smime_recipients = smime_files_row(
            "S/MIME-Empfängerzertifikate",
            "Zertifikate für Verschlüsselung; mehrere Dateien können ausgewählt werden",
            settings
                .as_ref()
                .map(|settings| settings.smime_recipient_certificates())
                .unwrap_or_default(),
            settings.clone(),
            |settings, values| settings.set_smime_recipient_certificates(values),
        );
        group_crypto.add(&row_smime_recipients);

        let row_smime_ca = smime_file_row(
            "S/MIME-Vertrauensspeicher (optional)",
            "CA-Bundle für die Vertrauensprüfung; ohne Angabe wird nur die Signaturmathematik geprüft",
            settings
                .as_ref()
                .map(|settings| settings.smime_ca_file())
                .unwrap_or_default(),
            settings.clone(),
            AppSettings::set_smime_ca_file,
        );
        group_crypto.add(&row_smime_ca);

        // GnuPG's machine-readable listing is loaded off the GTK main thread;
        // signing remains unavailable until the user has an actual key in the
        // local keyring, instead of pretending that a placeholder is usable.
        let pgp_keys_for_load = Rc::clone(&pgp_keys);
        let pgp_key_model_for_load = pgp_key_model.clone();
        let row_pgp_key_for_load = row_pgp_key.downgrade();
        let settings_for_key_load = settings.clone();
        let (key_sender, key_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = crypto_service::list_openpgp_keys();
            if let Err(error) = key_sender.send_blocking(result) {
                tracing::debug!(%error, "OpenPGP key list result was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let Ok(Ok(keys)) = key_receiver.recv().await else {
                return;
            };
            let selected_fingerprint = settings_for_key_load
                .as_ref()
                .map(|settings| settings.default_pgp_key());
            for key in keys {
                let label = if key.identity.trim().is_empty() {
                    key.fingerprint.clone()
                } else {
                    format!("{} · {}", key.identity, key.fingerprint)
                };
                pgp_key_model_for_load.append(&label);
                pgp_keys_for_load.borrow_mut().push(key);
            }
            if let Some(row) = row_pgp_key_for_load.upgrade()
                && let Some(fingerprint) = selected_fingerprint
                && let Some(index) = pgp_keys_for_load
                    .borrow()
                    .iter()
                    .position(|key| key.fingerprint.eq_ignore_ascii_case(&fingerprint))
            {
                row.set_selected(index as u32 + 1);
            }
        });

        let page_certificates = adw::PreferencesPage::builder()
            .title("Certificates")
            .build();
        page_certificates.add(&group_crypto);

        // ── Construct 2-Column Sidebar + Top Horizontal Sub-Tabs Layout ──────

        // Sub-Stack for Mail Preferences (General, HTML Messages, Labels, Headers, Junk, Confirmations)
        let mail_sub_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .hexpand(true)
            .vexpand(true)
            .build();
        let mail_sub_switcher = gtk::StackSwitcher::builder()
            .stack(&mail_sub_stack)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(8)
            .build();

        let page_mail_general = adw::PreferencesPage::builder().title("General").build();
        page_mail_general.add(&group_reading);
        let page_mail_html = adw::PreferencesPage::builder()
            .title("HTML Messages")
            .build();
        page_mail_html.add(&group_theme);
        let page_mail_labels = adw::PreferencesPage::builder().title("Labels").build();
        page_mail_labels.add(&group_mail_columns);
        let page_mail_headers = adw::PreferencesPage::builder().title("Headers").build();
        page_mail_headers.add(&group_mail_normal_headers);
        let page_mail_junk = adw::PreferencesPage::builder().title("Junk").build();
        page_mail_junk.add(&group_filters);
        let page_mail_confirmations = adw::PreferencesPage::builder()
            .title("Confirmations")
            .build();
        page_mail_confirmations.add(&group_mail_list);

        mail_sub_stack.add_titled(&page_mail_general, Some("general"), "General");
        mail_sub_stack.add_titled(&page_mail_html, Some("html"), "HTML Messages");
        mail_sub_stack.add_titled(&page_mail_labels, Some("labels"), "Labels");
        mail_sub_stack.add_titled(&page_mail_headers, Some("headers"), "Headers");
        mail_sub_stack.add_titled(&page_mail_junk, Some("junk"), "Junk");
        mail_sub_stack.add_titled(
            &page_mail_confirmations,
            Some("confirmations"),
            "Confirmations",
        );

        let mail_detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .vexpand(true)
            .build();
        mail_detail_box.append(&mail_sub_switcher);
        mail_detail_box.append(&mail_sub_stack);

        // Sub-Stack for Composer Preferences (General, Signatures, Replies)
        let composer_sub_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .hexpand(true)
            .vexpand(true)
            .build();
        let composer_sub_switcher = gtk::StackSwitcher::builder()
            .stack(&composer_sub_stack)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(8)
            .build();

        let page_composer_general = adw::PreferencesPage::builder().title("General").build();
        page_composer_general.add(&group_composer);
        let page_composer_signatures = adw::PreferencesPage::builder().title("Signatures").build();
        page_composer_signatures.add(&group_signature_editor);
        let page_composer_replies = adw::PreferencesPage::builder().title("Replies").build();
        page_composer_replies.add(&group_reply);

        composer_sub_stack.add_titled(&page_composer_general, Some("general"), "General");
        composer_sub_stack.add_titled(&page_composer_signatures, Some("signatures"), "Signatures");
        composer_sub_stack.add_titled(&page_composer_replies, Some("replies"), "Replies");

        let composer_detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .vexpand(true)
            .build();
        composer_detail_box.append(&composer_sub_switcher);
        composer_detail_box.append(&composer_sub_stack);

        // Sub-Stack for Calendar and Tasks (General, Display & Formats)
        let calendar_sub_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .hexpand(true)
            .vexpand(true)
            .build();
        let calendar_sub_switcher = gtk::StackSwitcher::builder()
            .stack(&calendar_sub_stack)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(8)
            .build();

        let page_calendar_general = adw::PreferencesPage::builder().title("General").build();
        page_calendar_general.add(&group_calendar_view);
        page_calendar_general.add(&group_calendar_days);
        page_calendar_general.add(&group_calendar_time);
        page_calendar_general.add(&group_calendar_reminders);

        let page_calendar_display = adw::PreferencesPage::builder()
            .title("Display &amp; Formats")
            .build();
        page_calendar_display.add(&group_default_ecal_sources);
        page_calendar_display.add(&group_categories);
        page_calendar_display.add(&group_task_memo_lists);

        calendar_sub_stack.add_titled(&page_calendar_general, Some("general"), "General");
        calendar_sub_stack.add_titled(&page_calendar_display, Some("display"), "Display & Formats");

        let calendar_detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .vexpand(true)
            .build();
        calendar_detail_box.append(&calendar_sub_switcher);
        calendar_detail_box.append(&calendar_sub_stack);

        // Sidebar Category List (Left Column)
        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        for (title, icon_name) in [
            ("Mail Accounts", "avatar-default-symbolic"),
            ("Contacts", "contact-new-symbolic"),
            ("Mail Preferences", "mail-unread-symbolic"),
            ("Composer Preferences", "document-edit-symbolic"),
            ("Network Preferences", "network-wired-symbolic"),
            ("Calendar and Tasks", "x-office-calendar-symbolic"),
            ("Certificates", "security-high-symbolic"),
            ("Appearance", "display-brightness-symbolic"),
            ("Import &amp; Backup", "drive-harddisk-symbolic"),
        ] {
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            let icon = gtk::Image::from_icon_name(icon_name);
            row.add_prefix(&icon);
            sidebar_list.append(&row);
        }

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .width_request(230)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&sidebar_list)
            .build();

        // Right Detail Stack
        let detail_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .hexpand(true)
            .vexpand(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        detail_stack.add_titled(&page_accounts, Some("accounts"), "Mail Accounts");
        detail_stack.add_titled(&page_contacts, Some("contacts"), "Contacts");
        detail_stack.add_titled(&mail_detail_box, Some("mail"), "Mail Preferences");
        detail_stack.add_titled(
            &composer_detail_box,
            Some("composer"),
            "Composer Preferences",
        );
        detail_stack.add_titled(&page_network, Some("network"), "Network Preferences");
        detail_stack.add_titled(&calendar_detail_box, Some("calendar"), "Calendar and Tasks");
        detail_stack.add_titled(&page_certificates, Some("certificates"), "Certificates");
        detail_stack.add_titled(&page_appearance, Some("appearance"), "Appearance");
        detail_stack.add_titled(&page_backup, Some("backup"), "Import & Backup");

        let detail_stack_weak = detail_stack.downgrade();
        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(stack) = detail_stack_weak.upgrade() else {
                return;
            };
            let name = match row.index() {
                0 => "accounts",
                1 => "contacts",
                2 => "mail",
                3 => "composer",
                4 => "network",
                5 => "calendar",
                6 => "certificates",
                7 => "appearance",
                8 => "backup",
                _ => "accounts",
            };
            stack.set_visible_child_name(name);
        });

        // Select Mail Preferences (index 2) by default
        if let Some(row) = sidebar_list.row_at_index(2) {
            sidebar_list.select_row(Some(&row));
        }

        let main_split = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .vexpand(true)
            .build();
        main_split.append(&sidebar_scroll);
        main_split.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        main_split.append(&detail_stack);

        let header_bar = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Evolution Preferences", ""))
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();
        content_box.append(&header_bar);
        content_box.append(&main_split);

        self.set_child(Some(&content_box));
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

const MAX_EDITABLE_MAIL_NORMAL_HEADERS: usize = 64;

fn mail_column_label(column: &str) -> &'static str {
    match column {
        "status" => "Status",
        "attachment" => "Anhang",
        "flagged" => "Markierung",
        "sender" => "Absender / Empfänger",
        "subject" => "Betreff",
        "date" => "Datum",
        "follow-up" => "Nachverfolgung",
        "important" => "Wichtigkeit",
        "avatar" => "Avatar",
        "preview" => "Vorschautext",
        "size" => "Größe",
        "follow-up-due" => "Fälligkeit",
        "location" => "Ort",
        "messages" => "Nachrichten (Wide)",
        _ => "Unbekannte Spalte",
    }
}

fn build_mail_column_order_editor(settings: Option<AppSettings>) -> gtk::Box {
    let state = Rc::new(RefCell::new(
        settings
            .as_ref()
            .map(AppSettings::mail_list_column_order)
            .unwrap_or_else(|| {
                crate::services::settings_service::MAIL_LIST_COLUMN_IDS
                    .iter()
                    .map(|column| (*column).to_string())
                    .collect()
            }),
    ));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();
    rebuild_mail_column_order_list(&list, &state, &settings);

    if let Some(settings) = settings.as_ref() {
        let state = Rc::clone(&state);
        let list = list.clone();
        let settings_for_signal = settings.clone();
        let settings_for_rows = Some(settings.clone());
        settings.connect_changed(Some("mail-list-column-order"), move |_, _| {
            state.replace(settings_for_signal.mail_list_column_order());
            rebuild_mail_column_order_list(&list, &state, &settings_for_rows);
        });
    }

    let editor = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(6)
        .build();
    editor.append(
        &gtk::Label::builder()
            .label("Spaltenreihenfolge")
            .halign(gtk::Align::Start)
            .css_classes(vec!["heading"])
            .build(),
    );
    editor.append(&list);
    editor
}

fn rebuild_mail_column_order_list(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<String>>>,
    settings: &Option<AppSettings>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let columns = state.borrow().clone();
    for (index, column) in columns.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(mail_column_label(column))
            .subtitle(column)
            .build();
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Spalte nach links verschieben")
            .css_classes(vec!["flat", "circular"])
            .sensitive(index > 0)
            .build();
        let down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Spalte nach rechts verschieben")
            .css_classes(vec!["flat", "circular"])
            .sensitive(index + 1 < columns.len())
            .build();
        row.add_suffix(&up);
        row.add_suffix(&down);

        for (button, target) in [(up, index.checked_sub(1)), (down, Some(index + 1))] {
            let state = Rc::clone(state);
            let list = list.clone();
            let settings = settings.clone();
            button.connect_clicked(move |_| {
                let Some(target) = target.filter(|target| *target < state.borrow().len()) else {
                    return;
                };
                state.borrow_mut().swap(index, target);
                if let Some(settings) = settings.as_ref() {
                    let order = state.borrow().clone();
                    settings.set_mail_list_column_order(&order);
                } else {
                    rebuild_mail_column_order_list(&list, &state, &settings);
                }
            });
        }
        list.append(&row);
    }
}

fn default_mail_normal_headers_for_editor() -> Vec<(String, bool)> {
    [
        ("From", true),
        ("Reply-To", true),
        ("To", true),
        ("Cc", true),
        ("Bcc", true),
        ("Subject", true),
        ("Date", true),
        ("Newsgroups", true),
        ("Face", true),
        ("x-evolution-mailer", false),
    ]
    .into_iter()
    .map(|(name, visible)| (name.to_string(), visible))
    .collect()
}

fn build_mail_normal_headers_editor(settings: Option<AppSettings>) -> gtk::Box {
    let state = Rc::new(RefCell::new(
        settings
            .as_ref()
            .map(AppSettings::mail_normal_headers)
            .unwrap_or_else(default_mail_normal_headers_for_editor),
    ));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();
    rebuild_mail_normal_headers_list(&list, &state, &settings);

    let add_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(6)
        .build();
    let entry = gtk::Entry::builder()
        .placeholder_text("Eigener Header, z. B. List-Id")
        .hexpand(true)
        .activates_default(true)
        .build();
    let add_button = gtk::Button::builder()
        .label("Hinzufügen")
        .css_classes(vec!["flat", "suggested-action"])
        .sensitive(false)
        .build();
    add_box.append(&entry);
    add_box.append(&add_button);

    let validation_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(vec!["error"])
        .visible(false)
        .build();

    let reset_button = gtk::Button::builder()
        .label("Zurücksetzen")
        .tooltip_text("Normale Header auf die Evolution-Defaultliste zurücksetzen")
        .css_classes(vec!["flat"])
        .halign(gtk::Align::Start)
        .build();

    let editor = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    editor.append(&list);
    editor.append(&add_box);
    editor.append(&validation_label);
    editor.append(&reset_button);

    let state_for_validation = Rc::clone(&state);
    let validation_for_entry = validation_label.clone();
    let add_button_for_entry = add_button.clone();
    entry.connect_changed(move |entry| {
        let validation = validate_new_mail_normal_header(&entry.text(), &state_for_validation);
        add_button_for_entry.set_sensitive(validation.is_ok());
        set_mail_header_validation_message(&validation_for_entry, validation.err());
    });

    let add_header = {
        let state = Rc::clone(&state);
        let list = list.clone();
        let settings = settings.clone();
        let validation_label = validation_label.clone();
        let entry = entry.clone();
        move || {
            add_mail_normal_header(&entry, &list, &state, &settings, &validation_label);
        }
    };
    let add_header_for_button = add_header.clone();
    add_button.connect_clicked(move |_| add_header_for_button());
    entry.connect_activate(move |_| add_header());

    let state_for_reset = Rc::clone(&state);
    let list_for_reset = list.clone();
    let settings_for_reset = settings.clone();
    let entry_for_reset = entry.clone();
    let validation_for_reset = validation_label.clone();
    reset_button.connect_clicked(move |_| {
        state_for_reset.replace(default_mail_normal_headers_for_editor());
        persist_mail_normal_headers(&state_for_reset, &settings_for_reset);
        rebuild_mail_normal_headers_list(&list_for_reset, &state_for_reset, &settings_for_reset);
        entry_for_reset.set_text("");
        set_mail_header_validation_message(&validation_for_reset, None);
    });

    editor
}

fn rebuild_mail_normal_headers_list(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let headers = state.borrow().clone();
    for (index, (name, visible)) in headers.into_iter().enumerate() {
        list.append(&mail_normal_header_row(
            list, state, settings, index, name, visible,
        ));
    }
}

fn mail_normal_header_row(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    index: usize,
    name: String,
    visible: bool,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(8)
        .margin_end(8)
        .build();
    let label = gtk::Label::builder()
        .label(&name)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let visibility = gtk::Switch::builder()
        .active(visible)
        .valign(gtk::Align::Center)
        .tooltip_text("Header in der normalen Leseansicht anzeigen")
        .build();
    let move_up =
        mail_header_editor_button("go-up-symbolic", "Header nach oben verschieben", index > 0);
    let move_down = mail_header_editor_button(
        "go-down-symbolic",
        "Header nach unten verschieben",
        index + 1 < state.borrow().len(),
    );
    let remove = mail_header_editor_button("user-trash-symbolic", "Header entfernen", true);

    content.append(&label);
    content.append(&visibility);
    content.append(&move_up);
    content.append(&move_down);
    content.append(&remove);
    row.set_child(Some(&content));

    let state_for_visibility = Rc::clone(state);
    let settings_for_visibility = settings.clone();
    let name_for_visibility = name.clone();
    visibility.connect_active_notify(move |switch| {
        let changed = {
            let mut headers = state_for_visibility.borrow_mut();
            if let Some((_, enabled)) = headers
                .iter_mut()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&name_for_visibility))
            {
                *enabled = switch.is_active();
                true
            } else {
                false
            }
        };
        if changed {
            persist_mail_normal_headers(&state_for_visibility, &settings_for_visibility);
        }
    });

    let state_for_up = Rc::clone(state);
    let settings_for_up = settings.clone();
    let list_for_up = list.clone();
    let name_for_up = name.clone();
    move_up.connect_clicked(move |_| {
        move_mail_normal_header(
            &list_for_up,
            &state_for_up,
            &settings_for_up,
            &name_for_up,
            -1,
        );
    });

    let state_for_down = Rc::clone(state);
    let settings_for_down = settings.clone();
    let list_for_down = list.clone();
    let name_for_down = name.clone();
    move_down.connect_clicked(move |_| {
        move_mail_normal_header(
            &list_for_down,
            &state_for_down,
            &settings_for_down,
            &name_for_down,
            1,
        );
    });

    let state_for_remove = Rc::clone(state);
    let settings_for_remove = settings.clone();
    let list_for_remove = list.clone();
    remove.connect_clicked(move |_| {
        state_for_remove
            .borrow_mut()
            .retain(|(candidate, _)| !candidate.eq_ignore_ascii_case(&name));
        persist_mail_normal_headers(&state_for_remove, &settings_for_remove);
        rebuild_mail_normal_headers_list(&list_for_remove, &state_for_remove, &settings_for_remove);
    });

    row
}

fn mail_header_editor_button(icon_name: &str, tooltip: &str, sensitive: bool) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon_name)
        .tooltip_text(tooltip)
        .css_classes(vec!["flat"])
        .sensitive(sensitive)
        .valign(gtk::Align::Center)
        .build()
}

fn move_mail_normal_header(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    name: &str,
    direction: isize,
) {
    let mut headers = state.borrow_mut();
    let Some(index) = headers
        .iter()
        .position(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
    else {
        return;
    };
    let target = index as isize + direction;
    if !(0..headers.len() as isize).contains(&target) {
        return;
    }
    headers.swap(index, target as usize);
    drop(headers);
    persist_mail_normal_headers(state, settings);
    rebuild_mail_normal_headers_list(list, state, settings);
}

fn persist_mail_normal_headers(
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
) {
    if let Some(settings) = settings.as_ref() {
        let headers = state.borrow().clone();
        settings.set_mail_normal_headers(&headers);
    }
}

fn validate_new_mail_normal_header(
    value: &str,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
) -> Result<String, String> {
    let name = validate_mail_normal_header_name(value)?;
    let headers = state.borrow();
    if headers.len() >= MAX_EDITABLE_MAIL_NORMAL_HEADERS {
        return Err(format!(
            "Es können höchstens {MAX_EDITABLE_MAIL_NORMAL_HEADERS} Header verwaltet werden."
        ));
    }
    if headers
        .iter()
        .any(|(candidate, _)| candidate.eq_ignore_ascii_case(&name))
    {
        return Err(format!("Der Header „{name}“ ist bereits vorhanden."));
    }
    Ok(name)
}

fn set_mail_header_validation_message(label: &gtk::Label, message: Option<String>) {
    if let Some(message) = message {
        label.set_label(&message);
        label.set_visible(true);
    } else {
        label.set_label("");
        label.set_visible(false);
    }
}

fn add_mail_normal_header(
    entry: &gtk::Entry,
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    validation_label: &gtk::Label,
) {
    let value = entry.text().to_string();
    let name = match validate_new_mail_normal_header(&value, state) {
        Ok(name) => name,
        Err(message) => {
            set_mail_header_validation_message(validation_label, Some(message));
            return;
        }
    };
    state.borrow_mut().push((name, true));
    persist_mail_normal_headers(state, settings);
    rebuild_mail_normal_headers_list(list, state, settings);
    entry.set_text("");
    set_mail_header_validation_message(validation_label, None);
}

fn smime_file_row(
    title: &str,
    subtitle: &str,
    initial: String,
    settings: Option<AppSettings>,
    set_value: fn(&AppSettings, &str),
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    let path_label = gtk::Label::builder()
        .label(if initial.trim().is_empty() {
            "Nicht festgelegt"
        } else {
            initial.as_str()
        })
        .hexpand(true)
        .halign(gtk::Align::End)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(36)
        .build();
    row.add_suffix(&path_label);

    let clear_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Pfad entfernen")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear_button);
    let choose_button = gtk::Button::builder()
        .label("Auswählen…")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&choose_button);

    let settings_for_clear = settings.clone();
    let path_label_for_clear = path_label.clone();
    clear_button.connect_clicked(move |_| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_value(settings, "");
        }
        path_label_for_clear.set_label("Nicht festgelegt");
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    choose_button.connect_clicked(move |button| {
        let Some(root) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("S/MIME-Datei auswählen")
            .modal(true)
            .build();
        let settings_for_result = settings_for_choose.clone();
        let path_label_for_result = path_label_for_choose.clone();
        file_dialog.open(Some(&root), None::<&gio::Cancellable>, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let value = path.to_string_lossy().into_owned();
            if let Some(settings) = settings_for_result.as_ref() {
                set_value(settings, &value);
            }
            path_label_for_result.set_label(&value);
        });
    });

    row
}

fn smime_files_row(
    title: &str,
    subtitle: &str,
    initial: Vec<String>,
    settings: Option<AppSettings>,
    set_values: fn(&AppSettings, &[String]),
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    let initial_text = initial.join("\n");
    let path_label = gtk::Label::builder()
        .label(if initial_text.trim().is_empty() {
            "Nicht festgelegt"
        } else {
            initial_text.as_str()
        })
        .hexpand(true)
        .halign(gtk::Align::End)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(36)
        .build();
    row.add_suffix(&path_label);

    let clear_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Zertifikatspfade entfernen")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear_button);
    let choose_button = gtk::Button::builder()
        .label("Auswählen…")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&choose_button);

    let settings_for_clear = settings.clone();
    let path_label_for_clear = path_label.clone();
    clear_button.connect_clicked(move |_| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_values(settings, &[]);
        }
        path_label_for_clear.set_label("Nicht festgelegt");
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    choose_button.connect_clicked(move |button| {
        let Some(root) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("S/MIME-Empfängerzertifikate auswählen")
            .modal(true)
            .build();
        let settings_for_result = settings_for_choose.clone();
        let path_label_for_result = path_label_for_choose.clone();
        file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
            let Ok(model) = result else {
                return;
            };
            let paths = selected_file_paths(&model)
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return;
            }
            if let Some(settings) = settings_for_result.as_ref() {
                set_values(settings, &paths);
            }
            path_label_for_result.set_label(&paths.join("\n"));
        });
    });

    row
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
                    refresh_open_category_views(&parent);
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
                        refresh_open_category_views(&parent);
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

fn refresh_open_category_views(parent: &PreferencesDialog) {
    if let Some(window) = parent
        .root()
        .and_downcast::<crate::window::EvolutionWindow>()
    {
        window.refresh_category_presentation();
    }
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
        FilterField::Unread => "Ungelesen",
        FilterField::Flagged => "Stern-/Markierungsstatus",
        FilterField::Important => "Wichtigkeit",
        FilterField::Attachment => "Anhang vorhanden",
        FilterField::Date => "Datum",
        FilterField::Folder => "Ordner",
        FilterField::Junk => "Spam/Junk",
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
        FilterOperator::Before => "vor",
        FilterOperator::After => "nach",
        FilterOperator::Exists => "existiert",
    }
}

fn filter_action_label(action: &FilterAction) -> String {
    action.description()
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ManualFilterRunSnapshot, ManualFilterSnapshotError, default_mail_normal_headers_for_editor,
        manual_filter_result_toast, validate_new_mail_normal_header,
    };
    use crate::services::mail_filter_service::{
        FilterAction, FilterCondition, FilterField, FilterOperator, FilterScope, MailFilterRule,
        MailFilterStore,
    };
    use crate::services::mail_store_service::{LocalFilterRunReport, MailStoreWorkerConfig};
    use std::{
        cell::RefCell,
        fs,
        io::ErrorKind,
        path::{Path, PathBuf},
        rc::Rc,
    };

    fn temporary_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "evolution-gtk4-preferences-{label}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    fn manual_worker_config(root: &Path, mail_dir: &Path) -> MailStoreWorkerConfig {
        MailStoreWorkerConfig {
            mail_dir_override: Some(mail_dir.to_path_buf()),
            local_mail_directory: None,
            metadata_path_override: Some(root.join("mail-state.json")),
            app_data_directory: root.join("app-data"),
        }
    }

    fn manual_rule(id: &str, action: FilterAction) -> MailFilterRule {
        MailFilterRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            scope: FilterScope::Manual,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: "invoice".to_string(),
                operator: FilterOperator::Contains,
            },
            action: action.clone(),
            conditions: Vec::new(),
            actions: vec![action],
            match_all: true,
        }
    }

    #[test]
    fn normal_header_editor_uses_evolution_default_order() {
        assert_eq!(
            default_mail_normal_headers_for_editor(),
            vec![
                ("From".to_string(), true),
                ("Reply-To".to_string(), true),
                ("To".to_string(), true),
                ("Cc".to_string(), true),
                ("Bcc".to_string(), true),
                ("Subject".to_string(), true),
                ("Date".to_string(), true),
                ("Newsgroups".to_string(), true),
                ("Face".to_string(), true),
                ("x-evolution-mailer".to_string(), false),
            ]
        );
    }

    #[test]
    fn normal_header_editor_reports_invalid_and_duplicate_names() {
        let state = Rc::new(RefCell::new(vec![("Subject".to_string(), true)]));
        assert!(validate_new_mail_normal_header("  X-Trace  ", &state).is_ok());
        assert!(validate_new_mail_normal_header("subject", &state).is_err());
        assert!(validate_new_mail_normal_header("X-Bad: value", &state).is_err());
    }

    #[test]
    fn local_account_rows_reorder_reconcile_defaults_and_keep_eds_read_only() {
        let source = include_str!("preferences_dialog.rs");
        assert!(source.contains("move_profile(&profile_id, -1)"));
        assert!(source.contains("move_profile(&profile_id, 1)"));
        assert!(source.contains("reconcile_local_default(&profile_id)"));
        assert!(source.contains("Standardkonto"));
        assert!(source.contains("schreibgeschützte EDS-Entdeckung"));
        assert!(!source.contains(concat!(
            "AccountSetupDialog::new_for_edit(&dummy_",
            "profile)"
        )));
    }

    #[test]
    fn manual_filter_worker_boundary_captures_inputs_before_spawn() {
        let source = include_str!("preferences_dialog.rs");
        let manual_section_start = source
            .find("tooltip_text(\"Regel jetzt im lokalen Mailbestand ausführen\")")
            .expect("manual filter row should exist");
        let manual_section = &source[manual_section_start..];
        let spawn = manual_section
            .find("std::thread::Builder::new()")
            .expect("manual filter should use a worker");
        let before_spawn = &manual_section[..spawn];
        let future_start = manual_section
            .find("let dialog_weak = dialog.downgrade()")
            .expect("manual filter result future should remain on GTK");
        let worker = &manual_section[spawn..future_start];

        assert!(before_spawn.contains("MailStoreWorkerConfig::capture"));
        assert!(before_spawn.contains("MailFilterStore::from_environment()"));
        assert!(before_spawn.contains("rule_id_for_run.clone()"));
        assert!(before_spawn.contains("rule_for_run.clone()"));
        assert!(before_spawn.contains("ManualFilterRunSnapshot::capture"));
        assert!(worker.contains("snapshot.run_on_worker()"));
        for forbidden in [
            "AppSettings",
            "gio::",
            "gtk::",
            "glib::",
            "adw::",
            "GObject",
            "std::env::",
            "app_data_dir(",
        ] {
            assert!(
                !worker.contains(forbidden),
                "manual filter worker must stay framework/global-state free: {forbidden}"
            );
        }

        // The result handling is deliberately kept in the GTK future: the
        // worker boundary must not absorb UI state or change user feedback.
        for marker in [
            "run_button_for_result.set_sensitive(false)",
            "run_button.set_sensitive(true)",
            "if !completion_allowed.get()",
            "connect_closed",
            "manual_filter_result_toast",
        ] {
            assert!(
                manual_section.contains(marker),
                "manual filter UI path lost {marker}"
            );
        }
    }

    #[test]
    fn manual_filter_snapshot_is_owned_and_keeps_the_selected_rule_exact() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}

        assert_send_sync_static::<ManualFilterRunSnapshot>();

        let root = temporary_directory("snapshot");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody\n",
        )
        .expect("mail should be written");

        let filter_path = root.join("mail-filters.json");
        let source_store = MailFilterStore::new(&filter_path);
        let selected_rule = manual_rule("late-rule", FilterAction::MarkRead);
        source_store
            .upsert(selected_rule.clone())
            .expect("selected rule should be written");
        let snapshot = ManualFilterRunSnapshot::capture(
            "late-rule",
            selected_rule.clone(),
            manual_worker_config(&root, &mail_dir),
            source_store.clone(),
        )
        .expect("selected rule should be captured");
        let snapshot_path = snapshot.filter_store.path().to_path_buf();
        assert_eq!(snapshot.rule_id, "late-rule");
        assert_eq!(snapshot.rule, selected_rule);
        assert_eq!(
            snapshot.store_config.mail_dir_override.as_deref(),
            Some(mail_dir.as_path())
        );
        assert_eq!(snapshot.filter_store.load(), vec![snapshot.rule.clone()]);

        // A later edit of the source store must not change the rule selected
        // by the already captured row.
        source_store
            .upsert(manual_rule("late-rule", FilterAction::Delete))
            .expect("later source edit should be written");

        let result = std::thread::spawn(move || snapshot.run_on_worker())
            .join()
            .expect("manual filter worker should return");

        let report = result
            .expect("manual filter should not fail")
            .expect("captured rule should be available");
        assert_eq!(report.matched_messages, 1);
        assert_eq!(report.deleted_messages, 0);
        assert!(
            !snapshot_path.exists(),
            "worker must clean its private store"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manual_filter_invalid_missing_and_io_errors_never_look_like_success() {
        let root = temporary_directory("errors");
        let mail_dir = root.join("mail");
        fs::create_dir_all(&mail_dir).expect("mail directory should be created");
        fs::write(
            mail_dir.join("invoice.eml"),
            "From: Billing <billing@example.org>\nSubject: Invoice\n\nBody\n",
        )
        .expect("mail should be written");

        let filter_path = root.join("mail-filters.json");
        let filter_store = MailFilterStore::new(filter_path.as_path());
        let missing = ManualFilterRunSnapshot::capture(
            "missing",
            manual_rule("missing", FilterAction::MarkRead),
            manual_worker_config(&root, &mail_dir),
            filter_store.clone(),
        )
        .expect_err("missing rule must stop before the worker");
        assert!(matches!(missing, ManualFilterSnapshotError::MissingRule));
        assert_eq!(missing.toast_message(), "Filterregel wurde nicht gefunden");

        let invalid_rule = MailFilterRule {
            id: "invalid".to_string(),
            name: "Invalid".to_string(),
            enabled: true,
            scope: FilterScope::Manual,
            condition: FilterCondition {
                field: FilterField::Subject,
                value: String::new(),
                operator: FilterOperator::Contains,
            },
            action: FilterAction::MoveToFolder(String::new()),
            conditions: Vec::new(),
            actions: Vec::new(),
            match_all: true,
        };
        filter_store
            .save(std::slice::from_ref(&invalid_rule))
            .expect("invalid fixture should be persisted for boundary validation");
        let invalid = ManualFilterRunSnapshot::capture(
            "invalid",
            invalid_rule,
            manual_worker_config(&root, &mail_dir),
            filter_store.clone(),
        )
        .expect_err("invalid rule must stop before the worker");
        assert!(matches!(invalid, ManualFilterSnapshotError::InvalidRule(_)));
        assert!(invalid.toast_message().contains("fehlgeschlagen"));
        assert!(!invalid.toast_message().contains("Treffer"));

        let valid_rule = manual_rule("broken-state", FilterAction::MarkRead);
        filter_store
            .upsert(valid_rule.clone())
            .expect("valid error-path rule should be written");
        let broken_state = root.join("state-directory");
        fs::create_dir(&broken_state).expect("broken state directory should be created");
        let mut error_config = manual_worker_config(&root, &mail_dir);
        error_config.metadata_path_override = Some(broken_state);
        let snapshot = ManualFilterRunSnapshot::capture(
            "broken-state",
            valid_rule,
            error_config,
            filter_store,
        )
        .expect("valid rule should be captured");
        let snapshot_path = snapshot.filter_store.path().to_path_buf();
        let worker_error = snapshot
            .run_on_worker()
            .expect_err("metadata write failure must reach the GTK result path");
        assert!(!worker_error.to_string().is_empty());
        assert!(
            !snapshot_path.exists(),
            "failed workers must clean snapshots"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manual_filter_result_toasts_cover_success_missing_error_and_cancel() {
        let success = Ok(Ok(Some(LocalFilterRunReport {
            matched_messages: 2,
            changed_messages: 1,
            moved_messages: 1,
            copied_messages: 0,
            deleted_messages: 0,
        })));
        assert_eq!(
            manual_filter_result_toast(&success),
            "2 Treffer · 1 geändert · 1 verschoben · 0 kopiert · 0 gelöscht"
        );

        let missing = Ok(Ok(None));
        assert_eq!(
            manual_filter_result_toast(&missing),
            "Filterregel wurde nicht gefunden"
        );

        let worker_error = Ok(Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "state unavailable",
        )));
        assert_eq!(
            manual_filter_result_toast(&worker_error),
            "Lokaler Filterlauf fehlgeschlagen: state unavailable"
        );

        let cancelled: Result<
            std::io::Result<Option<LocalFilterRunReport>>,
            async_channel::RecvError,
        > = Err(async_channel::RecvError);
        assert_eq!(
            manual_filter_result_toast(&cancelled),
            "Lokaler Filterlauf abgebrochen: receiving from an empty and closed channel"
        );
    }
}
