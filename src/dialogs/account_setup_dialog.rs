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
            .property("default-width", 660)
            .property("default-height", 660)
            .build()
    }

    pub fn new_for_edit(profile: &AccountProfile) -> Self {
        let encoded = serde_json::to_string(profile).expect("account profile should serialize");
        glib::Object::builder()
            .property("title", "Kontoprofil bearbeiten")
            .property("modal", true)
            .property("default-width", 660)
            .property("default-height", 660)
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
                "GNOME-Onlinekonto",
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

        let signatures = crate::services::signature_store_service::SignatureStore::default().load();
        let mut sig_labels = vec!["Keine (nur Text)".to_string(), "Allgemeine Standardsignatur".to_string()];
        for sig in &signatures {
            sig_labels.push(sig.name.clone());
        }
        let sig_refs: Vec<&str> = sig_labels.iter().map(String::as_str).collect();
        let row_signature = adw::ComboRow::builder()
            .title("Standard-Signatur")
            .subtitle("Signatur für ausgehende E-Mails dieses Kontos")
            .model(&gtk::StringList::new(&sig_refs))
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            if let Some(ref sig_id) = profile.signature_id {
                if sig_id == "default" {
                    row_signature.set_selected(1);
                } else if let Some(pos) = signatures.iter().position(|s| s.id == *sig_id) {
                    row_signature.set_selected((pos + 2) as u32);
                } else {
                    row_signature.set_selected(0);
                }
            } else {
                row_signature.set_selected(0);
            }
        }
        group_user.add(&row_signature);

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
            .title("Server-Einstellungen &amp; Abruf")
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

        let row_auto_check = adw::SwitchRow::builder()
            .title("Automatisch auf neue E-Mails prüfen")
            .active(edit_profile.as_ref().map(|p| p.auto_check_enabled).unwrap_or(true))
            .build();
        group_server.add(&row_auto_check);

        let row_check_interval = adw::EntryRow::builder()
            .title("Prüfintervall (Minuten)")
            .text(&edit_profile.as_ref().map(|p| p.check_interval_minutes.to_string()).unwrap_or_else(|| "10".to_string()))
            .build();
        group_server.add(&row_check_interval);

        let row_check_all_folders = adw::SwitchRow::builder()
            .title("In allen Ordnern nach neuen Nachrichten suchen")
            .subtitle("Prüft nicht nur den Posteingang")
            .active(edit_profile.as_ref().map(|p| p.check_all_folders).unwrap_or(false))
            .build();
        group_server.add(&row_check_all_folders);

        let row_expunge_on_exit = adw::SwitchRow::builder()
            .title("Papierkorb beim Beenden bereinigen (Expunge)")
            .active(edit_profile.as_ref().map(|p| p.expunge_on_exit).unwrap_or(false))
            .build();
        group_server.add(&row_expunge_on_exit);

        content_box.append(&group_server);

        // ── 3b. Folder mapping ────────────────────────────────────────────
        let group_folders = adw::PreferencesGroup::builder()
            .title("Ordnerzuordnung")
            .description(
                "Nicht-geheime Ordnernamen; die native Kontoanbindung wird auf Linux geprüft",
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
            "Wird nur bei einer sicheren Kontoanbindung verwendet und nicht lokal gespeichert",
        ));
        group_cred.add(&row_password);

        content_box.append(&group_cred);

        // ── 5. Security Settings ─────────────────────────────────────────────
        let group_security = adw::PreferencesGroup::builder()
            .title("Sicherheit &amp; Verschlüsselung")
            .description("OpenPGP- und S/MIME-Einstellungen für dieses Konto")
            .build();

        let row_pgp_key = adw::EntryRow::builder()
            .title("OpenPGP-Schlüssel-ID")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_pgp_key.set_text(&profile.pgp_key_id);
        }
        group_security.add(&row_pgp_key);

        let row_pgp_sign = adw::SwitchRow::builder()
            .title("Ausgehende E-Mails immer mit PGP signieren")
            .active(edit_profile.as_ref().map(|p| p.pgp_always_sign).unwrap_or(false))
            .build();
        group_security.add(&row_pgp_sign);

        let row_pgp_encrypt = adw::SwitchRow::builder()
            .title("Ausgehende E-Mails immer mit PGP verschlüsseln")
            .active(edit_profile.as_ref().map(|p| p.pgp_always_encrypt).unwrap_or(false))
            .build();
        group_security.add(&row_pgp_encrypt);

        let row_smime_sign = adw::EntryRow::builder()
            .title("S/MIME-Signaturzertifikat")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_smime_sign.set_text(&profile.smime_sign_cert);
        }
        group_security.add(&row_smime_sign);

        let row_smime_encrypt = adw::EntryRow::builder()
            .title("S/MIME-Verschlüsselungszertifikat")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_smime_encrypt.set_text(&profile.smime_encrypt_cert);
        }
        group_security.add(&row_smime_encrypt);

        content_box.append(&group_security);

        // ── 6. Composing Settings ────────────────────────────────────────────
        let group_composing = adw::PreferencesGroup::builder()
            .title("Optionen für das Verfassen")
            .description("Standardeinstellungen für neue E-Mails von diesem Konto")
            .build();

        let row_prefer_html = adw::SwitchRow::builder()
            .title("HTML-Formatierung beim Verfassen bevorzugen")
            .active(edit_profile.as_ref().map(|p| p.prefer_html).unwrap_or(true))
            .build();
        group_composing.add(&row_prefer_html);

        let row_default_cc = adw::EntryRow::builder()
            .title("Immer Kopie (CC) an")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_default_cc.set_text(&profile.default_cc);
        }
        group_composing.add(&row_default_cc);

        let row_default_bcc = adw::EntryRow::builder()
            .title("Immer Blindkopie (BCC) an")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_default_bcc.set_text(&profile.default_bcc);
        }
        group_composing.add(&row_default_bcc);

        content_box.append(&group_composing);

        let update_protocol_fields = {
            let row_imap_host = row_imap_host.clone();
            let row_smtp_host = row_smtp_host.clone();
            let row_imap_port = row_imap_port.clone();
            let row_smtp_port = row_smtp_port.clone();
            let row_security = row_security.clone();
            let row_inbox_folder = row_inbox_folder.clone();
            let row_drafts_folder = row_drafts_folder.clone();
            let row_sent_folder = row_sent_folder.clone();
            let row_archive_folder = row_archive_folder.clone();
            let row_trash_folder = row_trash_folder.clone();
            let row_junk_folder = row_junk_folder.clone();
            let row_username = row_username.clone();
            let row_auth_method = row_auth_method.clone();
            let row_password = row_password.clone();
            move |selected: u32| {
                let is_exchange = selected == 1;
                let is_goa = selected == 2;
                let is_pop3 = selected == 3;
                let has_manual_servers = !is_goa;
                let has_manual_smtp = !is_goa && !is_exchange;

                row_imap_host.set_sensitive(has_manual_servers);
                row_imap_host.set_title(if is_exchange {
                    "Exchange-/EWS-Server"
                } else if is_pop3 {
                    "Posteingangsserver (POP3)"
                } else if is_goa {
                    "Posteingangsserver (über das Onlinekonto verwaltet)"
                } else {
                    "Posteingangsserver (IMAP)"
                });
                row_imap_port.set_sensitive(has_manual_servers);
                row_smtp_host.set_sensitive(has_manual_smtp);
                row_smtp_port.set_sensitive(has_manual_smtp);
                row_security.set_sensitive(has_manual_servers);

                for row in [
                    &row_inbox_folder,
                    &row_drafts_folder,
                    &row_sent_folder,
                    &row_archive_folder,
                    &row_trash_folder,
                    &row_junk_folder,
                ] {
                    row.set_sensitive(!is_goa);
                }

                row_username.set_sensitive(!is_goa);
                row_auth_method.set_sensitive(!is_goa);
                row_password.set_sensitive(!is_goa);
                if is_goa {
                    row_username.set_tooltip_text(Some(
                        "Das Onlinekonto verwaltet Benutzername und Zugangsdaten",
                    ));
                    row_auth_method.set_text("Onlinekonto / OAuth2");
                } else {
                    row_username.set_tooltip_text(None);
                }
            }
        };
        update_protocol_fields(combo_type.selected());
        combo_type.connect_selected_notify(move |combo| {
            update_protocol_fields(combo.selected());
        });

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
            row_signature,
            #[strong]
            row_imap_host,
            #[strong]
            row_smtp_host,
            #[strong]
            row_imap_port,
            #[strong]
            row_smtp_port,
            #[strong]
            row_auto_check,
            #[strong]
            row_check_interval,
            #[strong]
            row_check_all_folders,
            #[strong]
            row_expunge_on_exit,
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
            #[strong]
            row_pgp_key,
            #[strong]
            row_pgp_sign,
            #[strong]
            row_pgp_encrypt,
            #[strong]
            row_smime_sign,
            #[strong]
            row_smime_encrypt,
            #[strong]
            row_prefer_html,
            #[strong]
            row_default_cc,
            #[strong]
            row_default_bcc,
            move |_| {
                let email = row_email.text().trim().to_string();
                if email.is_empty() {
                    tracing::warn!("Cannot save an account profile without an email address");
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window
                                .add_toast(&adw::Toast::new("Bitte eine E-Mail-Adresse eingeben"));
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
                    if name.is_empty() { email.clone() } else { name }
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

                let signatures = crate::services::signature_store_service::SignatureStore::default().load();
                let signature_id = match row_signature.selected() {
                    0 => None,
                    1 => Some("default".to_string()),
                    idx => signatures.get((idx - 2) as usize).map(|s| s.id.clone()),
                };

                let check_interval_minutes = row_check_interval
                    .text()
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(10)
                    .clamp(1, 1440);

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
                    signature_id,
                    check_interval_minutes,
                    auto_check_enabled: row_auto_check.is_active(),
                    check_all_folders: row_check_all_folders.is_active(),
                    expunge_on_exit: row_expunge_on_exit.is_active(),
                    pgp_key_id: row_pgp_key.text().trim().to_string(),
                    pgp_always_sign: row_pgp_sign.is_active(),
                    pgp_always_encrypt: row_pgp_encrypt.is_active(),
                    smime_sign_cert: row_smime_sign.text().trim().to_string(),
                    smime_encrypt_cert: row_smime_encrypt.text().trim().to_string(),
                    default_cc: row_default_cc.text().trim().to_string(),
                    default_bcc: row_default_bcc.text().trim().to_string(),
                    prefer_html: row_prefer_html.is_active(),
                };

                if let Err(error) = profile.validate() {
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window.add_toast(&adw::Toast::new(&error));
                        }
                    }
                    return;
                }

                let profile_id = profile.id.clone();
                match AccountConfigStore::default().upsert(profile) {
                    Ok(()) => {
                        if let Some(settings) = AppSettings::try_new() {
                            if settings.default_account_id().trim().is_empty() {
                                settings.set_default_account_id(&profile_id);
                            }
                        }
                        tracing::info!("Account profile saved; account synchronization is pending");
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.add_toast(&adw::Toast::new(
                                    "Kontoprofil gespeichert; Kontosynchronisation steht noch aus",
                                ));
                            }
                        }
                        dialog.close();
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist account profile");
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
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
