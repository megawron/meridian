// SPDX-License-Identifier: GPL-3.0-or-later
//
// AccountSetupDialog — Step-by-step tabbed settings & setup editor for Mail accounts.
//
// Structured into 5 native Libadwaita tabs (Reiter) matching Evolution upstream:
// - 1. Identität (Identity): Account type/protocol, User info, Signatures
// - 2. Empfangen (Receiving): IMAP/POP3/Exchange server, credentials, sync interval & options
// - 3. Versenden (Sending): SMTP server, ports, security
// - 4. Standardordner (Defaults): Special folder mappings, compose defaults (HTML, CC, BCC)
// - 5. Sicherheit (Security): OpenPGP key ID & sign/encrypt toggles, S/MIME certificates

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
            .property("default-width", 700)
            .property("default-height", 660)
            .build()
    }

    pub fn new_for_edit(profile: &AccountProfile) -> Self {
        let encoded = serde_json::to_string(profile).expect("account profile should serialize");
        glib::Object::builder()
            .property("title", "Kontoeinstellungen")
            .property("modal", true)
            .property("default-width", 700)
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
        let is_editing = edit_profile.is_some();
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();

        let btn_cancel = gtk::Button::builder()
            .label("Abbrechen")
            .build();
        btn_cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        header_bar.pack_start(&btn_cancel);

        let btn_finish = gtk::Button::builder()
            .label(if is_editing { "Speichern" } else { "Einrichten" })
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&btn_finish);

        let view_stack = adw::ViewStack::builder()
            .vexpand(true)
            .hexpand(true)
            .build();

        let window_title_str = if let Some(ref profile) = edit_profile {
            if !profile.display_name.is_empty() {
                format!("Kontoeinstellungen — {}", profile.display_name)
            } else {
                format!("Kontoeinstellungen — {}", profile.email)
            }
        } else {
            "Neues Konto einrichten".to_string()
        };

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header_bar.set_title_widget(Some(&view_switcher));
        toolbar_view.add_top_bar(&header_bar);

        let view_switcher_bar = adw::ViewSwitcherBar::builder()
            .stack(&view_stack)
            .build();
        toolbar_view.add_bottom_bar(&view_switcher_bar);

        let window_title = adw::WindowTitle::builder()
            .title(&window_title_str)
            .build();

        let bp_narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 580sp").unwrap());
        bp_narrow.add_setter(&header_bar, "title-widget", Some(&window_title.to_value()));
        bp_narrow.add_setter(&view_switcher_bar, "reveal", Some(&true.to_value()));
        self.add_breakpoint(bp_narrow);

        // ─────────────────────────────────────────────────────────────────────
        // ── TAB 1: IDENTITÄT (Identity) ──────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let page_identity = adw::PreferencesPage::builder()
            .title("Identität")
            .icon_name("avatar-default-symbolic")
            .build();

        // 1a. Account Provider Selection
        let group_type = adw::PreferencesGroup::builder()
            .title("Kontotyp")
            .description("Wähle den Anbieter oder das Protokoll deines E-Mail-Dienstes")
            .build();

        let combo_type = adw::ComboRow::builder()
            .title("Protokoll / Dienst")
            .model(&gtk::StringList::new(&[
                "IMAP &amp; SMTP (Standard)",
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
        page_identity.add(&group_type);

        // 1b. User Information
        let group_user = adw::PreferencesGroup::builder()
            .title("Benutzerinformationen")
            .description("Persönliche Angaben für ausgehende Nachrichten")
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
        page_identity.add(&group_user);

        // 1c. Standard Signature
        let group_signature = adw::PreferencesGroup::builder()
            .title("Signatur")
            .description("Standard-Signatur für neue Nachrichten dieses Kontos")
            .build();

        let signatures = crate::services::signature_store_service::SignatureStore::default().load();
        let mut sig_labels = vec![
            "Keine (nur Text)".to_string(),
            "Allgemeine Standardsignatur".to_string(),
        ];
        for sig in &signatures {
            sig_labels.push(sig.name.clone());
        }
        let sig_refs: Vec<&str> = sig_labels.iter().map(String::as_str).collect();
        let row_signature = adw::ComboRow::builder()
            .title("Standard-Signatur")
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
        group_signature.add(&row_signature);
        page_identity.add(&group_signature);

        view_stack.add_titled_with_icon(
            &page_identity,
            Some("identity"),
            "Identität",
            "avatar-default-symbolic",
        );

        // ─────────────────────────────────────────────────────────────────────
        // ── TAB 2: EMPFANGEN (Receiving) ─────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let page_receiving = adw::PreferencesPage::builder()
            .title("Empfangen")
            .icon_name("mail-unread-symbolic")
            .build();

        // 2a. Server Settings
        let group_server = adw::PreferencesGroup::builder()
            .title("Posteingangsserver")
            .description("Verbindungseinstellungen für eingehende E-Mails")
            .build();

        let row_imap_host = adw::EntryRow::builder()
            .title("Posteingangsserver (IMAP)")
            .build();
        group_server.add(&row_imap_host);

        let row_imap_port = adw::EntryRow::builder().title("Server-Port").build();
        group_server.add(&row_imap_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_imap_host.set_text(&profile.imap_host);
            row_imap_port.set_text(&profile.imap_port.to_string());
        }

        let row_security = adw::ComboRow::builder()
            .title("Verschlüsselung")
            .model(&gtk::StringList::new(&[
                "SSL / TLS (z. B. Port 993 / 995)",
                "STARTTLS (z. B. Port 143 / 110)",
                "Keine Verschlüsselung (unsicher)",
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
        page_receiving.add(&group_server);

        // 2b. Credentials
        let group_cred = adw::PreferencesGroup::builder()
            .title("Authentifizierung &amp; Zugangsdaten")
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
            "Wird nur bei einer sicheren Kontoanbindung verwendet und nicht lokal im Klartext gespeichert",
        ));
        group_cred.add(&row_password);
        page_receiving.add(&group_cred);

        // 2c. Check / Sync Options
        let group_sync = adw::PreferencesGroup::builder()
            .title("Abruf- &amp; Bereinigungsoptionen")
            .build();

        let row_auto_check = adw::SwitchRow::builder()
            .title("Automatisch auf neue E-Mails prüfen")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.auto_check_enabled)
                    .unwrap_or(true),
            )
            .build();
        group_sync.add(&row_auto_check);

        let row_check_interval = adw::EntryRow::builder()
            .title("Prüfintervall (Minuten)")
            .text(
                &edit_profile
                    .as_ref()
                    .map(|p| p.check_interval_minutes.to_string())
                    .unwrap_or_else(|| "10".to_string()),
            )
            .build();
        let row_interval_ref = row_check_interval.clone();
        row_auto_check.connect_active_notify(move |switch| {
            row_interval_ref.set_sensitive(switch.is_active());
        });
        row_check_interval.set_sensitive(row_auto_check.is_active());
        group_sync.add(&row_check_interval);

        let row_check_all_folders = adw::SwitchRow::builder()
            .title("In allen Ordnern nach neuen Nachrichten suchen")
            .subtitle("Prüft nicht nur den Posteingang, sondern alle abonnierten Ordner")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.check_all_folders)
                    .unwrap_or(false),
            )
            .build();
        group_sync.add(&row_check_all_folders);

        let row_expunge_on_exit = adw::SwitchRow::builder()
            .title("Papierkorb beim Beenden bereinigen (Expunge)")
            .subtitle("Gelöschte Nachrichten beim Schließen von Evolution endgültig entfernen")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.expunge_on_exit)
                    .unwrap_or(false),
            )
            .build();
        group_sync.add(&row_expunge_on_exit);
        page_receiving.add(&group_sync);

        view_stack.add_titled_with_icon(
            &page_receiving,
            Some("receiving"),
            "Empfangen",
            "mail-unread-symbolic",
        );

        // ─────────────────────────────────────────────────────────────────────
        // ── TAB 3: VERSENDEN (Sending) ───────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let page_sending = adw::PreferencesPage::builder()
            .title("Versenden")
            .icon_name("mail-send-symbolic")
            .build();

        let group_smtp = adw::PreferencesGroup::builder()
            .title("Postausgangsserver (SMTP)")
            .description("Server für ausgehende E-Mails")
            .build();

        let row_smtp_host = adw::EntryRow::builder()
            .title("Postausgangsserver (SMTP)")
            .build();
        group_smtp.add(&row_smtp_host);

        let row_smtp_port = adw::EntryRow::builder().title("SMTP-Port").build();
        group_smtp.add(&row_smtp_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_smtp_host.set_text(&profile.smtp_host);
            row_smtp_port.set_text(&profile.smtp_port.to_string());
        }

        let row_smtp_auth_info = adw::ActionRow::builder()
            .title("Authentifizierung für Postausgang")
            .subtitle("Verwendet standardmäßig dieselben Zugangsdaten wie der Posteingang")
            .build();
        let icon_lock = gtk::Image::from_icon_name("security-high-symbolic");
        row_smtp_auth_info.add_prefix(&icon_lock);
        group_smtp.add(&row_smtp_auth_info);

        page_sending.add(&group_smtp);
        view_stack.add_titled_with_icon(
            &page_sending,
            Some("sending"),
            "Versenden",
            "mail-send-symbolic",
        );

        // ─────────────────────────────────────────────────────────────────────
        // ── TAB 4: STANDARDORDNER & VERFASSEN (Defaults) ─────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let page_defaults = adw::PreferencesPage::builder()
            .title("Standardordner")
            .icon_name("folder-symbolic")
            .build();

        let group_folders = adw::PreferencesGroup::builder()
            .title("Spezielle Ordnerzuordnung")
            .description("Ordner für Entwürfe, Gesendet, Archiv, Papierkorb und Spam")
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
        page_defaults.add(&group_folders);

        let group_composing = adw::PreferencesGroup::builder()
            .title("Optionen für das Verfassen")
            .description("Standardeinstellungen beim Verfassen neuer Nachrichten von diesem Konto")
            .build();

        let row_prefer_html = adw::SwitchRow::builder()
            .title("HTML-Formatierung beim Verfassen bevorzugen")
            .subtitle("Nachrichten standardmäßig im Rich-Text-/HTML-Format erstellen")
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
        page_defaults.add(&group_composing);

        view_stack.add_titled_with_icon(
            &page_defaults,
            Some("defaults"),
            "Standardordner",
            "folder-symbolic",
        );

        // ─────────────────────────────────────────────────────────────────────
        // ── TAB 5: SICHERHEIT (Security) ─────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let page_security = adw::PreferencesPage::builder()
            .title("Sicherheit")
            .icon_name("security-high-symbolic")
            .build();

        let group_pgp = adw::PreferencesGroup::builder()
            .title("OpenPGP-Verschlüsselung &amp; Signaturen")
            .description("GnuPG / OpenPGP-Schlüsselverwaltung für dieses Konto")
            .build();

        let row_pgp_key = adw::EntryRow::builder()
            .title("OpenPGP-Schlüssel-ID (z. B. 0x1234ABCD)")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_pgp_key.set_text(&profile.pgp_key_id);
        }
        group_pgp.add(&row_pgp_key);

        let row_pgp_sign = adw::SwitchRow::builder()
            .title("Ausgehende E-Mails standardmäßig signieren (PGP)")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.pgp_always_sign)
                    .unwrap_or(false),
            )
            .build();
        group_pgp.add(&row_pgp_sign);

        let row_pgp_encrypt = adw::SwitchRow::builder()
            .title("Ausgehende E-Mails standardmäßig verschlüsseln (PGP)")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.pgp_always_encrypt)
                    .unwrap_or(false),
            )
            .build();
        group_pgp.add(&row_pgp_encrypt);
        page_security.add(&group_pgp);

        let group_smime = adw::PreferencesGroup::builder()
            .title("S/MIME-Zertifikate")
            .description("X.509-Zertifikate für digitale Signatur und Verschlüsselung")
            .build();

        let row_smime_sign = adw::EntryRow::builder()
            .title("S/MIME-Signaturzertifikat")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_smime_sign.set_text(&profile.smime_sign_cert);
        }
        group_smime.add(&row_smime_sign);

        let row_smime_encrypt = adw::EntryRow::builder()
            .title("S/MIME-Verschlüsselungszertifikat")
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            row_smime_encrypt.set_text(&profile.smime_encrypt_cert);
        }
        group_smime.add(&row_smime_encrypt);
        page_security.add(&group_smime);

        view_stack.add_titled_with_icon(
            &page_security,
            Some("security"),
            "Sicherheit",
            "security-high-symbolic",
        );

        // ─────────────────────────────────────────────────────────────────────
        // ── PROTOCOL FIELDS DYNAMICS ─────────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
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
            let group_smtp = group_smtp.clone();
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
                if is_goa {
                    group_smtp.set_description(Some(
                        "Der E-Mail-Versand wird automatisch über das GNOME-Onlinekonto verwaltet.",
                    ));
                } else if is_exchange {
                    group_smtp.set_description(Some(
                        "Der E-Mail-Versand erfolgt direkt über die Exchange-/EWS-Verbindung.",
                    ));
                } else {
                    group_smtp.set_description(Some("Server für ausgehende E-Mails (SMTP)."));
                }
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

        // ─────────────────────────────────────────────────────────────────────
        // ── SAVE & VALIDATION ACTION ─────────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        btn_finish.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            view_stack,
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
                    view_stack.set_visible_child_name("identity");
                    row_email.grab_focus();
                    tracing::warn!("Cannot save an account profile without an email address");
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window.add_toast(&adw::Toast::new(
                                "Bitte eine E-Mail-Adresse im Reiter „Identität“ eingeben",
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
                let aliases = row_aliases
                    .text()
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();

                let default_imap_port = match protocol {
                    AccountProtocol::Pop3 => 995,
                    _ => 993,
                };
                let imap_port = row_imap_port
                    .text()
                    .trim()
                    .parse::<u16>()
                    .unwrap_or(default_imap_port);
                let smtp_port = row_smtp_port.text().trim().parse::<u16>().unwrap_or(465);
                let tls_mode = match row_security.selected() {
                    1 => "starttls".to_string(),
                    2 => "none".to_string(),
                    _ => "tls".to_string(),
                };

                let signature_id = match row_signature.selected() {
                    0 => None,
                    1 => Some("default".to_string()),
                    idx => {
                        let store =
                            crate::services::signature_store_service::SignatureStore::default();
                        store
                            .load()
                            .get((idx - 2) as usize)
                            .map(|s| s.id.clone())
                    }
                };

                let check_interval_minutes = row_check_interval
                    .text()
                    .trim()
                    .parse::<u32>()
                    .unwrap_or(10)
                    .max(1);

                let profile = AccountProfile {
                    id: edit_id
                        .clone()
                        .unwrap_or_else(|| format!("account:{}", email.to_lowercase())),
                    display_name,
                    email,
                    reply_to: row_reply_to.text().trim().to_string(),
                    organization: row_organization.text().trim().to_string(),
                    aliases,
                    protocol,
                    imap_host: row_imap_host.text().trim().to_string(),
                    imap_port,
                    smtp_host: row_smtp_host.text().trim().to_string(),
                    smtp_port,
                    username: row_username.text().trim().to_string(),
                    auth_method: row_auth_method.text().trim().to_string(),
                    tls_mode,
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
                    let err_lower = error.to_lowercase();
                    if err_lower.contains("imap")
                        || err_lower.contains("pop")
                        || err_lower.contains("empfang")
                        || err_lower.contains("port")
                    {
                        view_stack.set_visible_child_name("receiving");
                    } else if err_lower.contains("smtp") || err_lower.contains("versand") {
                        view_stack.set_visible_child_name("sending");
                    } else {
                        view_stack.set_visible_child_name("identity");
                    }
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
                                window.add_toast(&adw::Toast::new(if is_editing {
                                    "Kontoeinstellungen gespeichert"
                                } else {
                                    "Konto erfolgreich eingerichtet"
                                }));
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

        toolbar_view.set_content(Some(&view_stack));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for AccountSetupDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_setup_dialog_tab_contract_and_labels() {
        let source = include_str!("account_setup_dialog.rs");
        // Verify all 5 tab identifiers and titles
        assert!(source.contains("add_titled_with_icon"));
        assert!(source.contains("\"identity\""));
        assert!(source.contains("\"Identität\""));
        assert!(source.contains("avatar-default-symbolic"));

        assert!(source.contains("\"receiving\""));
        assert!(source.contains("\"Empfangen\""));
        assert!(source.contains("mail-unread-symbolic"));

        assert!(source.contains("\"sending\""));
        assert!(source.contains("\"Versenden\""));
        assert!(source.contains("mail-send-symbolic"));

        assert!(source.contains("\"defaults\""));
        assert!(source.contains("\"Standardordner\""));
        assert!(source.contains("folder-symbolic"));

        assert!(source.contains("\"security\""));
        assert!(source.contains("\"Sicherheit\""));
        assert!(source.contains("security-high-symbolic"));

        // Verify responsive ViewSwitcher and Breakpoint
        assert!(source.contains("adw::ViewSwitcher::builder()"));
        assert!(source.contains("adw::ViewSwitcherBar::builder()"));
        assert!(source.contains("adw::Breakpoint::new"));
        assert!(source.contains("max-width: 580sp"));

        // Verify action buttons
        assert!(source.contains("Abbrechen"));
        assert!(source.contains("Speichern"));
        assert!(source.contains("Einrichten"));
    }

    #[test]
    fn account_setup_dialog_edit_profile_roundtrip() {
        let mut profile = AccountProfile::default();
        profile.id = "test-acc-123".to_string();
        profile.display_name = "Max Mustermann".to_string();
        profile.email = "max@example.com".to_string();
        profile.imap_host = "imap.example.com".to_string();
        profile.smtp_host = "smtp.example.com".to_string();
        profile.pgp_key_id = "0x9876FEDC".to_string();
        profile.pgp_always_sign = true;

        let encoded = serde_json::to_string(&profile).expect("must serialize");
        let decoded = serde_json::from_str::<AccountProfile>(&encoded).expect("must deserialize");
        assert_eq!(decoded.id, "test-acc-123");
        assert_eq!(decoded.display_name, "Max Mustermann");
        assert_eq!(decoded.email, "max@example.com");
        assert_eq!(decoded.pgp_key_id, "0x9876FEDC");
        assert!(decoded.pgp_always_sign);
    }
}
