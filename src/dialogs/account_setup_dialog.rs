// SPDX-License-Identifier: GPL-3.0-or-later
//
// AccountSetupDialog — Step-by-step setup wizard for Mail/Groupware accounts.
//
// Supports:
// - Account Type selection (IMAP/SMTP, Exchange EWS, Microsoft 365, GOA)
// - Server & Connection Settings (IMAP Host, Port, TLS, Auth)
// - Credentials & Test Connection

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;

use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, AccountProtocol,
};
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct AccountSetupDialog {
        pub edit_profile_json: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AccountSetupDialog {
        const NAME: &'static str = "EvolutionAccountSetupDialog";
        type Type = super::AccountSetupDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for AccountSetupDialog {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES
                .get_or_init(|| vec![glib::ParamSpecString::builder("edit-profile-json").build()])
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            if pspec.name() == "edit-profile-json" {
                *self.edit_profile_json.borrow_mut() = value.get().expect("string property");
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "edit-profile-json" => self.edit_profile_json.borrow().to_value(),
                _ => glib::Value::from_type(pspec.value_type()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for AccountSetupDialog {}
    impl WindowImpl for AccountSetupDialog {}
    impl AdwWindowImpl for AccountSetupDialog {}
}

glib::wrapper! {
    pub struct AccountSetupDialog(ObjectSubclass<imp::AccountSetupDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl AccountSetupDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Neues Konto einrichten")
            .property("modal", true)
            .property("default-width", 600)
            .property("default-height", 580)
            .build()
    }

    pub fn new_for_edit(profile: &AccountProfile) -> Self {
        let encoded = serde_json::to_string(profile).expect("account profile should serialize");
        glib::Object::builder()
            .property("title", "Kontoprofil bearbeiten")
            .property("modal", true)
            .property("default-width", 600)
            .property("default-height", 580)
            .property("edit-profile-json", encoded)
            .build()
    }

    fn build_ui(&self) {
        let edit_profile = self
            .imp()
            .edit_profile_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<AccountProfile>(raw).ok());
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_finish = gtk::Button::builder()
            .label("Einrichten")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_finish);
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

        // ── 1. Account Provider Selection ──────────────────────────────────
        let group_type = adw::PreferencesGroup::builder()
            .title("Kontotyp")
            .description("Wähle den Anbieter oder das Protokoll deines E-Mail-Dienstes")
            .build();

        let combo_type = adw::ComboRow::builder()
            .title("Protokoll / Dienst")
            .model(&gtk::StringList::new(&[
                "IMAP & SMTP (Standard)",
                "Microsoft Exchange (EWS / M365)",
                "GNOME Online Accounts (GOA)",
                "POP3 (Lokale E-Mails)",
            ]))
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            combo_type.set_selected(match profile.protocol {
                AccountProtocol::Exchange => 1,
                AccountProtocol::Goa => 2,
                AccountProtocol::Pop3 => 3,
                AccountProtocol::ImapSmtp => 0,
            });
        }
        group_type.add(&combo_type);
        content_box.append(&group_type);

        // ── 2. User Info ────────────────────────────────────────────────────
        let group_user = adw::PreferencesGroup::builder()
            .title("Benutzerinformationen")
            .build();

        let row_fullname = adw::EntryRow::builder().title("Vollständiger Name").build();
        group_user.add(&row_fullname);

        let row_email = adw::EntryRow::builder().title("E-Mail-Adresse").build();
        group_user.add(&row_email);

        let row_reply_to = adw::EntryRow::builder()
            .title("Antwortadresse (Reply-To)")
            .build();
        group_user.add(&row_reply_to);

        let row_organization = adw::EntryRow::builder().title("Organisation").build();
        group_user.add(&row_organization);

        let row_aliases = adw::EntryRow::builder()
            .title("Alias-Adressen (Komma getrennt)")
            .build();
        group_user.add(&row_aliases);

        if let Some(profile) = edit_profile.as_ref() {
            row_fullname.set_text(&profile.display_name);
            row_email.set_text(&profile.email);
            row_reply_to.set_text(&profile.reply_to);
            row_organization.set_text(&profile.organization);
            row_aliases.set_text(&profile.aliases.join(", "));
        }

        content_box.append(&group_user);

        // ── 3. Server Settings (IMAP / SMTP) ───────────────────────────────
        let group_server = adw::PreferencesGroup::builder()
            .title("Server-Einstellungen")
            .build();

        let row_imap_host = adw::EntryRow::builder()
            .title("Posteingangsserver (IMAP)")
            .build();
        group_server.add(&row_imap_host);

        let row_smtp_host = adw::EntryRow::builder()
            .title("Postausgangsserver (SMTP)")
            .build();
        group_server.add(&row_smtp_host);

        let row_imap_port = adw::EntryRow::builder().title("IMAP-Port").build();
        group_server.add(&row_imap_port);

        let row_smtp_port = adw::EntryRow::builder().title("SMTP-Port").build();
        group_server.add(&row_smtp_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_imap_host.set_text(&profile.imap_host);
            row_smtp_host.set_text(&profile.smtp_host);
            row_imap_port.set_text(&profile.imap_port.to_string());
            row_smtp_port.set_text(&profile.smtp_port.to_string());
        }

        let row_security = adw::ComboRow::builder()
            .title("Verschlüsselung")
            .model(&gtk::StringList::new(&[
                "SSL / TLS (Port 993 / 465)",
                "STARTTLS (Port 143 / 587)",
                "Keine Verschlüsselung",
            ]))
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_security.set_selected(match profile.tls_mode.as_str() {
                "starttls" => 1,
                "none" => 2,
                _ => 0,
            });
        }
        group_server.add(&row_security);

        content_box.append(&group_server);

        // ── 3b. Folder mapping ────────────────────────────────────────────
        let group_folders = adw::PreferencesGroup::builder()
            .title("Ordnerzuordnung")
            .description(
                "Nicht-geheime Evolution-Ordnernamen; die native EDS/Camel-Anwendung wird auf Linux geprüft",
            )
            .build();
        let row_inbox_folder = adw::EntryRow::builder().title("Posteingang").build();
        let row_drafts_folder = adw::EntryRow::builder().title("Entwürfe").build();
        let row_sent_folder = adw::EntryRow::builder().title("Gesendet").build();
        let row_archive_folder = adw::EntryRow::builder().title("Archiv").build();
        let row_trash_folder = adw::EntryRow::builder().title("Papierkorb").build();
        let row_junk_folder = adw::EntryRow::builder().title("Spam / Junk").build();
        for row in [
            &row_inbox_folder,
            &row_drafts_folder,
            &row_sent_folder,
            &row_archive_folder,
            &row_trash_folder,
            &row_junk_folder,
        ] {
            group_folders.add(row);
        }
        if let Some(profile) = edit_profile.as_ref() {
            row_inbox_folder.set_text(&profile.inbox_folder);
            row_drafts_folder.set_text(&profile.drafts_folder);
            row_sent_folder.set_text(&profile.sent_folder);
            row_archive_folder.set_text(&profile.archive_folder);
            row_trash_folder.set_text(&profile.trash_folder);
            row_junk_folder.set_text(&profile.junk_folder);
        }
        content_box.append(&group_folders);

        // ── 4. Credentials ──────────────────────────────────────────────────
        let group_cred = adw::PreferencesGroup::builder()
            .title("Authentifizierung")
            .build();

        let row_username = adw::EntryRow::builder().title("Benutzername").build();
        if let Some(profile) = edit_profile.as_ref() {
            row_username.set_text(&profile.username);
        }
        group_cred.add(&row_username);

        let row_auth_method = adw::EntryRow::builder()
            .title("Authentifizierungsverfahren")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_auth_method.set_text(&profile.auth_method);
        }
        group_cred.add(&row_auth_method);

        let row_password = adw::PasswordEntryRow::builder().title("Passwort").build();
        row_password.set_text("");
        row_password.set_tooltip_text(Some(
            "Wird erst bei einer EDS/Camel-Anbindung an den Secret Service übergeben; nicht lokal gespeichert",
        ));
        group_cred.add(&row_password);

        content_box.append(&group_cred);

        let edit_id = edit_profile.as_ref().map(|profile| profile.id.clone());
        let edit_enabled = edit_profile
            .as_ref()
            .map(|profile| profile.enabled)
            .unwrap_or(true);

        btn_finish.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            combo_type,
            #[strong]
            row_fullname,
            #[strong]
            row_email,
            #[strong]
            row_reply_to,
            #[strong]
            row_organization,
            #[strong]
            row_aliases,
            #[strong]
            row_imap_host,
            #[strong]
            row_smtp_host,
            #[strong]
            row_imap_port,
            #[strong]
            row_smtp_port,
            #[strong]
            row_username,
            #[strong]
            row_auth_method,
            #[strong]
            row_security,
            #[strong]
            row_inbox_folder,
            #[strong]
            row_drafts_folder,
            #[strong]
            row_sent_folder,
            #[strong]
            row_archive_folder,
            #[strong]
            row_trash_folder,
            #[strong]
            row_junk_folder,
            move |_| {
                let email = row_email.text().trim().to_string();
                if email.is_empty() {
                    tracing::warn!("Cannot save an account profile without an email address");
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window.add_toast(&adw::Toast::new(
                                "Bitte eine E-Mail-Adresse eingeben",
                            ));
                        }
                    }
                    return;
                }

                let protocol = match combo_type.selected() {
                    1 => AccountProtocol::Exchange,
                    2 => AccountProtocol::Goa,
                    3 => AccountProtocol::Pop3,
                    _ => AccountProtocol::ImapSmtp,
                };
                let display_name = {
                    let name = row_fullname.text().trim().to_string();
                    if name.is_empty() {
                        email.clone()
                    } else {
                        name
                    }
                };
                let tls_mode = match row_security.selected() {
                    1 => "starttls",
                    2 => "none",
                    _ => "tls",
                };
                let fallback_imap_port = if tls_mode == "starttls" { 143 } else { 993 };
                let fallback_smtp_port = if tls_mode == "starttls" { 587 } else { 465 };
                let parse_port = |text: &str, fallback: u16| {
                    text.trim()
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port > 0)
                        .unwrap_or(fallback)
                };
                let aliases = row_aliases
                    .text()
                    .split(',')
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                let profile = AccountProfile {
                    id: edit_id
                        .clone()
                        .unwrap_or_else(|| format!("account:{}", email.to_lowercase())),
                    display_name,
                    email,
                    protocol,
                    imap_host: row_imap_host.text().trim().to_string(),
                    smtp_host: row_smtp_host.text().trim().to_string(),
                    tls_mode: tls_mode.to_string(),
                    username: row_username.text().trim().to_string(),
                    imap_port: parse_port(&row_imap_port.text(), fallback_imap_port),
                    smtp_port: parse_port(&row_smtp_port.text(), fallback_smtp_port),
                    auth_method: row_auth_method.text().trim().to_string(),
                    reply_to: row_reply_to.text().trim().to_string(),
                    organization: row_organization.text().trim().to_string(),
                    aliases,
                    inbox_folder: row_inbox_folder.text().trim().to_string(),
                    drafts_folder: row_drafts_folder.text().trim().to_string(),
                    sent_folder: row_sent_folder.text().trim().to_string(),
                    archive_folder: row_archive_folder.text().trim().to_string(),
                    trash_folder: row_trash_folder.text().trim().to_string(),
                    junk_folder: row_junk_folder.text().trim().to_string(),
                    enabled: edit_enabled,
                };

                let profile_id = profile.id.clone();
                match AccountConfigStore::default().upsert(profile) {
                    Ok(()) => {
                        if let Some(settings) = AppSettings::try_new() {
                            if settings.default_account_id().trim().is_empty() {
                                settings.set_default_account_id(&profile_id);
                            }
                        }
                        tracing::info!(
                            "Account profile saved; EDS/Camel activation is still pending"
                        );
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                                window.add_toast(&adw::Toast::new(
                                    "Kontoprofil gespeichert; EDS/Camel-Synchronisation steht noch aus",
                                ));
                            }
                        }
                        dialog.close();
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist account profile");
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                                window.add_toast(&adw::Toast::new(&format!(
                                    "Kontoprofil konnte nicht gespeichert werden: {error}"
                                )));
                            }
                        }
                    }
                }
            }
        ));

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for AccountSetupDialog {
    fn default() -> Self {
        Self::new()
    }
}
