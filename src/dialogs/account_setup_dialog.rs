// SPDX-License-Identifier: GPL-3.0-or-later
//
// AccountSetupDialog — Step-by-step setup wizard for new accounts & two-column settings editor for existing accounts.
//
// Structured into 5 native Libadwaita steps/pages matching Evolution upstream:
// - 1. Identität (Identity): Account type/protocol, User info, Signatures (with Optional expander)
// - 2. Empfangen (Receiving): IMAP/POP3/Exchange server, credentials, OAB & Fetch URL, passwords, sync options
// - 3. Versenden (Sending): SMTP server, ports, security (skipped for Exchange EWS)
// - 4. Optionen / Standardordner (Options/Defaults): Sync options, interval, special folder mappings
// - 5. Zusammenfassung & Sicherheit (Summary/Security): Overview of configuration, OpenPGP & S/MIME

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, AccountProtocol,
};
use crate::services::settings_service::AppSettings;

pub const TAB_PAGES: [(&str, &str, &str); 5] = [
    ("identity", "Identität", "avatar-default-symbolic"),
    ("receiving", "Empfangen", "mail-unread-symbolic"),
    ("sending", "Versenden", "mail-send-symbolic"),
    ("defaults", "Standardordner", "folder-symbolic"),
    ("security", "Sicherheit", "security-high-symbolic"),
];

pub const WIZARD_STEPS_IMAP: [&str; 5] = ["identity", "receiving", "sending", "defaults", "security"];
pub const WIZARD_STEPS_EXCHANGE: [&str; 4] = ["identity", "receiving", "defaults", "security"];

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct AccountSetupDialog {
        pub edit_profile_json: RefCell<Option<String>>,
        pub sidebar_list: RefCell<Option<gtk::ListBox>>,
        pub view_stack: RefCell<Option<adw::ViewStack>>,
        pub split_view: RefCell<Option<adw::NavigationSplitView>>,
        pub toast_overlay: RefCell<Option<adw::ToastOverlay>>,
        pub discovery_generation: std::cell::Cell<u64>,
        pub discovery_cancellation: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub debounce_source: RefCell<Option<glib::SourceId>>,
        pub last_discovered_email: RefCell<String>,
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
                .get_or_init(|| vec![glib::ParamSpecString::builder("edit-profile-json").construct().build()])
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
    impl WindowImpl for AccountSetupDialog {
        fn close_request(&self) -> glib::Propagation {
            if let Some(cancellation) = self.discovery_cancellation.borrow().as_ref() {
                cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(source) = self.debounce_source.borrow_mut().take() {
                source.remove();
            }
            self.parent_close_request()
        }
    }
    impl AdwWindowImpl for AccountSetupDialog {}
}

glib::wrapper! {
    pub struct AccountSetupDialog(ObjectSubclass<imp::AccountSetupDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

/// Validates whether an email string is syntactically complete and eligible for autodiscovery.
pub(crate) fn is_discovery_candidate(value: &str) -> Option<(&str, &str)> {
    let trimmed = value.trim();
    if trimmed.chars().filter(|&c| c == '@').count() != 1 {
        return None;
    }
    let (local, domain) = trimmed.split_once('@')?;
    let local = local.trim();
    let domain = domain.trim();

    if local.is_empty()
        || domain.is_empty()
        || trimmed.chars().any(char::is_whitespace)
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return None;
    }

    // TLD must have at least 2 characters (e.g. .de, .com, .org)
    let tld = domain.rsplit('.').next()?;
    if tld.len() < 2 {
        return None;
    }

    Some((local, domain))
}

impl AccountSetupDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Neues Konto einrichten")
            .property("modal", true)
            .property("default-width", 880)
            .property("default-height", 680)
            .build()
    }

    pub fn new_for_edit(profile: &AccountProfile) -> Self {
        let encoded = serde_json::to_string(profile).expect("account profile should serialize");
        glib::Object::builder()
            .property("title", "Kontoeinstellungen")
            .property("modal", true)
            .property("default-width", 880)
            .property("default-height", 680)
            .property("edit-profile-json", encoded)
            .build()
    }

    pub fn add_toast(&self, toast: &adw::Toast) {
        if let Some(overlay) = self.imp().toast_overlay.borrow().as_ref() {
            overlay.add_toast(toast.clone());
        } else if let Some(window) = crate::window::find_evolution_window(self) {
            window.add_toast(toast);
        }
    }

    pub fn select_page(&self, target: &str) {
        if let Some(stack) = self.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name(target);
        }
        if let Some(list) = self.imp().sidebar_list.borrow().as_ref() {
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let page_name = unsafe {
                    row.data::<String>("page-name")
                        .map(|s| s.as_ref().clone())
                        .unwrap_or_default()
                };
                if page_name == target {
                    list.select_row(Some(&row));
                    break;
                }
                i += 1;
            }
        }
        if let Some(split) = self.imp().split_view.borrow().as_ref() {
            split.set_show_content(true);
        }
    }

    fn build_ui(&self) {
        let edit_profile = self
            .imp()
            .edit_profile_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<AccountProfile>(raw).ok());
        let is_editing = edit_profile.is_some();

        // ─────────────────────────────────────────────────────────────────────
        // ── 1. FORM FIELDS (Shared between Wizard and Preferences) ───────────
        // ─────────────────────────────────────────────────────────────────────

        // 1a. Account Type & Identity
        let protocol_labels = if cfg!(target_os = "macos") {
            [
                "IMAP und SMTP (Standard)",
                "Microsoft Exchange (EWS)",
                "macOS Internetaccounts (Kalender, Erinnerungen & Kontakte)",
                "POP3 und SMTP",
            ]
        } else {
            [
                "IMAP und SMTP (Standard)",
                "Microsoft Exchange (EWS)",
                "GNOME-Onlinekonto (GOA)",
                "POP3 und SMTP",
            ]
        };

        let combo_type = adw::ComboRow::builder()
            .title("Kontotyp")
            .model(&gtk::StringList::new(&protocol_labels))
            .use_subtitle(true)
            .css_classes(vec!["account-type-combo"])
            .build();

        let expression = gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            gtk::Expression::NONE,
            "string",
        );
        combo_type.set_expression(Some(expression));

        let list_factory = gtk::SignalListItemFactory::new();
        list_factory.connect_setup(|_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let hbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .margin_start(16)
                .margin_end(16)
                .margin_top(10)
                .margin_bottom(10)
                .build();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::None)
                .build();
            let check = gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .visible(false)
                .build();
            hbox.append(&label);
            hbox.append(&check);
            list_item.set_child(Some(&hbox));
        });
        list_factory.connect_bind(|_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(obj) = list_item.item().and_then(|i| i.downcast::<gtk::StringObject>().ok()) {
                if let Some(hbox) = list_item.child().and_then(|c| c.downcast::<gtk::Box>().ok()) {
                    if let Some(label) = hbox.first_child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                        label.set_text(&obj.string());
                    }
                    if let Some(check) = hbox.last_child().and_then(|c| c.downcast::<gtk::Image>().ok()) {
                        check.set_visible(list_item.is_selected());
                    }
                }
            }
        });
        combo_type.set_list_factory(Some(&list_factory));

        let update_combo_subtitle = {
            let combo_type_ref = combo_type.clone();
            move |idx: u32| {
                let text = match idx {
                    1 => "Microsoft Exchange (EWS)",
                    2 => {
                        if cfg!(target_os = "macos") {
                            "macOS Internetaccounts"
                        } else {
                            "GNOME-Onlinekonto (GOA)"
                        }
                    }
                    3 => "POP3 und SMTP",
                    _ => "IMAP und SMTP (Standard)",
                };
                combo_type_ref.set_subtitle(text);
            }
        };

        if let Some(profile) = edit_profile.as_ref() {
            let sel = match profile.protocol {
                AccountProtocol::Exchange => 1,
                AccountProtocol::Goa => 2,
                AccountProtocol::Pop3 => 3,
                AccountProtocol::ImapSmtp => 0,
            };
            combo_type.set_selected(sel);
            update_combo_subtitle(sel);
        } else {
            combo_type.set_selected(0);
            update_combo_subtitle(0);
        }

        let update_sub_cb = update_combo_subtitle.clone();
        combo_type.connect_selected_notify(move |combo| {
            update_sub_cb(combo.selected());
        });

        let group_type = adw::PreferencesGroup::builder()
            .title("Kontotyp")
            .description("Wählen Sie das gewünschte Verbindungsprotokoll aus")
            .build();
        group_type.add(&combo_type);

        #[cfg(target_os = "macos")]
        {
            let internet_accounts = adw::ActionRow::builder()
                .title("macOS Internetaccounts verwalten…")
                .subtitle(
                    "Kalender, Erinnerungen und Kontakte werden als native Systemquellen verwendet. Mail benötigt eine eigene Anmeldung in dieser App.",
                )
                .activatable(true)
                .build();
            let open_button = gtk::Button::builder()
                .label("Systemeinstellungen öffnen")
                .valign(gtk::Align::Center)
                .build();
            open_button.connect_clicked(|_| {
                crate::services::MacosIntegrationService::new().open_internet_accounts();
            });
            internet_accounts.connect_activated(|_| {
                crate::services::MacosIntegrationService::new().open_internet_accounts();
            });
            internet_accounts.add_suffix(&open_button);
            group_type.add(&internet_accounts);
        }

        let group_user = adw::PreferencesGroup::builder()
            .title("Benutzerinformationen")
            .description("Persönliche Angaben für ausgehende Nachrichten")
            .build();

        let row_fullname = adw::EntryRow::builder().title("Vollständiger Name").build();
        group_user.add(&row_fullname);

        let row_email = adw::EntryRow::builder().title("E-Mail-Adresse").build();

        // ── Autodiscover Status Indicator & Spinner ───────────────────────────
        let discovery_spinner = gtk::Spinner::builder()
            .spinning(false)
            .visible(false)
            .valign(gtk::Align::Center)
            .tooltip_text("Servereinstellungen werden ermittelt…")
            .build();
        row_email.add_suffix(&discovery_spinner);
        group_user.add(&row_email);

        let row_discovery_status = adw::ActionRow::builder()
            .title("Servereinstellungen")
            .subtitle("Geben Sie Ihre E-Mail-Adresse ein, um Einstellungen automatisch zu ermitteln")
            .visible(false)
            .build();
        let icon_discovery = gtk::Image::builder()
            .icon_name("network-server-symbolic")
            .pixel_size(16)
            .valign(gtk::Align::Center)
            .build();
        row_discovery_status.add_prefix(&icon_discovery);

        let btn_redetect = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .valign(gtk::Align::Center)
            .has_frame(false)
            .tooltip_text("Servereinstellungen erneut ermitteln")
            .visible(false)
            .build();
        row_discovery_status.add_suffix(&btn_redetect);
        group_user.add(&row_discovery_status);

        // 1c. Optional Information (ExpanderRow to keep UI calm and non-overwhelming)
        let expander_optional = adw::ExpanderRow::builder()
            .title("Optionale Informationen")
            .subtitle("Antwortadresse, Organisation, Aliase und Signatur")
            .expanded(false)
            .build();

        let row_reply_to = adw::EntryRow::builder()
            .title("Antwortadresse (Reply-To)")
            .build();
        expander_optional.add_row(&row_reply_to);

        let row_organization = adw::EntryRow::builder().title("Organisation").build();
        expander_optional.add_row(&row_organization);

        let row_aliases = adw::EntryRow::builder()
            .title("Alias-Adressen (Komma getrennt)")
            .build();
        expander_optional.add_row(&row_aliases);

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
        expander_optional.add_row(&row_signature);

        if let Some(profile) = edit_profile.as_ref() {
            row_fullname.set_text(&profile.display_name);
            row_email.set_text(&profile.email);
            row_reply_to.set_text(&profile.reply_to);
            row_organization.set_text(&profile.organization);
            row_aliases.set_text(&profile.aliases.join(", "));

            if !profile.reply_to.is_empty()
                || !profile.organization.is_empty()
                || !profile.aliases.is_empty()
                || profile.signature_id.is_some()
            {
                expander_optional.set_expanded(true);
            }
        }
        group_user.add(&expander_optional);

        // 1b. Server Settings (Receiving)
        let group_server = adw::PreferencesGroup::builder()
            .title("Posteingangsserver")
            .description("Verbindungseinstellungen für eingehende E-Mails")
            .build();

        let row_imap_host = adw::EntryRow::builder()
            .title("Posteingangsserver (IMAP)")
            .build();
        group_server.add(&row_imap_host);

        // Exchange EWS Discovery action row
        let row_fetch_url = adw::ActionRow::builder()
            .title("Exchange-Webdienste ermitteln")
            .subtitle("EWS- und OAB-Endpunkte über Server und Anmeldedaten ermitteln")
            .build();
        let btn_fetch = gtk::Button::builder()
            .label("URL abrufen")
            .valign(gtk::Align::Center)
            .css_classes(vec!["suggested-action"])
            .build();
        row_fetch_url.add_suffix(&btn_fetch);
        group_server.add(&row_fetch_url);

        // OAB URL Row (Offline Address Book)
        let row_oab_url = adw::EntryRow::builder()
            .title("OAB-URL (Offline-Adressbuch, optional)")
            .build();
        group_server.add(&row_oab_url);

        let row_open_other_mailbox = adw::SwitchRow::builder()
            .title("Freigegebenes Postfach / Gruppenpostfach öffnen")
            .subtitle("Für Team- und Gruppenpostfächer (z. B. edv@...), die über persönliche Zugangsdaten geöffnet werden")
            .active(false)
            .build();
        group_server.add(&row_open_other_mailbox);

        let row_other_mailbox_user = adw::EntryRow::builder()
            .title("E-Mail-Adresse des Gruppenpostfachs")
            .build();
        group_server.add(&row_other_mailbox_user);

        let row_other_user_ref = row_other_mailbox_user.clone();
        row_open_other_mailbox.connect_active_notify(move |switch| {
            row_other_user_ref.set_sensitive(switch.is_active());
        });
        row_other_mailbox_user.set_sensitive(false);

        let row_imap_port = adw::EntryRow::builder().title("Server-Port").build();
        group_server.add(&row_imap_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_imap_host.set_text(&profile.imap_host);
            row_oab_url.set_text(&profile.oab_url);
            row_imap_port.set_text(&profile.imap_port.to_string());

            let is_diff_user = !profile.username.trim().is_empty()
                && !profile.email.trim().is_empty()
                && profile.username.trim().to_lowercase() != profile.email.trim().to_lowercase();
            let should_open_other = profile.open_mailbox_other_user
                || (!profile.foreign_mailbox.trim().is_empty())
                || (profile.protocol == AccountProtocol::Exchange && is_diff_user);

            row_open_other_mailbox.set_active(should_open_other);
            row_other_mailbox_user.set_sensitive(should_open_other);
            if !profile.foreign_mailbox.trim().is_empty() {
                row_other_mailbox_user.set_text(&profile.foreign_mailbox);
            } else if should_open_other {
                row_other_mailbox_user.set_text(&profile.email);
            }
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

        // 1c. Credentials
        let group_cred = adw::PreferencesGroup::builder()
            .title("Authentifizierung und Zugangsdaten")
            .description("Zugangsdaten für die Anmeldung am E-Mail-Server")
            .build();

        let row_username = adw::EntryRow::builder().title("Benutzername").build();
        if let Some(profile) = edit_profile.as_ref() {
            row_username.set_text(&profile.username);
        }
        group_cred.add(&row_username);

        let row_auth_combo = adw::ComboRow::builder()
            .title("Authentifizierungsverfahren")
            .model(&gtk::StringList::new(&[
                "NTLM / Windows-Authentifizierung",
                "Standard-Passwort (Basic / Plain)",
                "Kerberos / GSSAPI",
                "OAuth2 / Modern Auth (M365)",
            ]))
            .build();
        if let Some(profile) = edit_profile.as_ref() {
            match profile.auth_method.to_lowercase().as_str() {
                "plain" | "basic" | "password" => row_auth_combo.set_selected(1),
                "gssapi" | "kerberos" => row_auth_combo.set_selected(2),
                "oauth2" | "modern" => row_auth_combo.set_selected(3),
                _ => row_auth_combo.set_selected(0),
            }
        }
        group_cred.add(&row_auth_combo);

        let row_check_auth = adw::ActionRow::builder()
            .title("Authentifizierungsarten")
            .subtitle("Unterstützte Verfahren vom Server abfragen")
            .build();
        let btn_check_auth = gtk::Button::builder()
            .label("Unterstützte Arten prüfen")
            .valign(gtk::Align::Center)
            .build();
        row_check_auth.add_suffix(&btn_check_auth);
        group_cred.add(&row_check_auth);

        let row_password = adw::PasswordEntryRow::builder()
            .title("Passwort")
            .build();
        row_password.set_tooltip_text(Some(
            "Wird sicher im GNOME-Schlüsselbund (Secret Service) hinterlegt",
        ));
        group_cred.add(&row_password);

        let row_remember_password = adw::SwitchRow::builder()
            .title("Passwort im Schlüsselbund speichern")
            .subtitle("Zugangsdaten sicher im GNOME-Schlüsselbund (Secret Service) hinterlegen")
            .active(true)
            .build();
        group_cred.add(&row_remember_password);

        // Action: Check supported authentication mechanisms for Exchange / EWS
        {
            let row_imap_host_ref = row_imap_host.clone();
            let row_check_auth_ref = row_check_auth.clone();
            let row_auth_combo_ref = row_auth_combo.clone();
            let btn_check_auth_ref = btn_check_auth.clone();
            let dialog_weak = self.downgrade();

            btn_check_auth.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };

                let host = row_imap_host_ref.text().trim().to_string();
                if host.is_empty() {
                    row_check_auth_ref.set_subtitle("⚠️ Bitte geben Sie zuerst die Rechneradresse (Host-URL) ein.");
                    row_imap_host_ref.grab_focus();
                    dialog.add_toast(&adw::Toast::new("Bitte zuerst die Rechneradresse (Host-URL) eingeben"));
                    return;
                }

                btn_check_auth_ref.set_sensitive(false);
                row_check_auth_ref.set_subtitle("Server wird nach unterstützten Authentifizierungsarten abgefragt …");

                let (tx, rx) = async_channel::bounded::<Result<Vec<String>, String>>(1);
                std::thread::spawn(move || {
                    let result = crate::services::account_config_service::query_exchange_auth_methods(&host);
                    let _ = tx.send_blocking(result);
                });

                let row_check_auth_clone = row_check_auth_ref.clone();
                let row_auth_combo_clone = row_auth_combo_ref.clone();
                let btn_check_auth_clone = btn_check_auth_ref.clone();
                let dialog_weak_inner = dialog.downgrade();

                glib::spawn_future_local(async move {
                    btn_check_auth_clone.set_sensitive(true);
                    if let Ok(res) = rx.recv().await {
                        match res {
                            Ok(methods) => {
                                let summary = methods.join(", ");
                                let mut selected_name = "";
                                if methods.iter().any(|m| m.contains("NTLM")) {
                                    row_auth_combo_clone.set_selected(0);
                                    selected_name = "NTLM";
                                } else if methods.iter().any(|m| m.contains("Kerberos")) {
                                    row_auth_combo_clone.set_selected(2);
                                    selected_name = "Kerberos";
                                } else if methods.iter().any(|m| m.contains("OAuth2")) {
                                    row_auth_combo_clone.set_selected(3);
                                    selected_name = "OAuth2";
                                } else if methods.iter().any(|m| m.contains("Basic")) {
                                    row_auth_combo_clone.set_selected(1);
                                    selected_name = "Basic";
                                }
                                row_check_auth_clone.set_subtitle(&format!("✓ Unterstützt: {summary} ({selected_name} gewählt)"));
                                if let Some(dlg) = dialog_weak_inner.upgrade() {
                                    dlg.add_toast(&adw::Toast::new(&format!("Unterstützte Verfahren: {summary}")));
                                }
                            }
                            Err(err) => {
                                row_check_auth_clone.set_subtitle(&format!("❌ {err}"));
                                if let Some(dlg) = dialog_weak_inner.upgrade() {
                                    dlg.add_toast(&adw::Toast::new("Authentifizierungsprüfung fehlgeschlagen"));
                                }
                            }
                        }
                    }
                });
            });
        }

        // 1d. Action: Authenticated Fetch URL for Exchange EWS with Authentication Request popup
        {
            let row_imap_host_ref = row_imap_host.clone();
            let row_oab_url_ref = row_oab_url.clone();
            let row_open_other_mailbox_ref = row_open_other_mailbox.clone();
            let row_other_mailbox_user_ref = row_other_mailbox_user.clone();
            let row_fetch_url_ref = row_fetch_url.clone();
            let row_email_ref = row_email.clone();
            let row_username_ref = row_username.clone();
            let row_password_ref = row_password.clone();
            let row_remember_password_ref = row_remember_password.clone();
            let row_check_auth_ref = row_check_auth.clone();
            let row_auth_combo_ref = row_auth_combo.clone();
            let btn_fetch_ref = btn_fetch.clone();
            let dialog_weak = self.downgrade();

            btn_fetch.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };

                let email = row_email_ref.text().trim().to_string();
                let mut initial_user = row_username_ref.text().trim().to_string();
                if initial_user.is_empty() {
                    initial_user = email.clone();
                }
                let initial_pwd = row_password_ref.text().to_string();

                let alert = adw::AlertDialog::new(
                    Some("Legitimierungsabfrage"),
                    Some("Geben Sie Ihre Anmeldedaten für das Exchange-Konto ein, um die Exchange-Webdienste (EWS & OAB) abzurufen:"),
                );

                let content_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();

                let group_prompt = adw::PreferencesGroup::new();

                let entry_user = adw::EntryRow::builder()
                    .title("Benutzername")
                    .text(&initial_user)
                    .build();
                group_prompt.add(&entry_user);

                let entry_pwd = adw::PasswordEntryRow::builder()
                    .title("Passwort")
                    .text(&initial_pwd)
                    .build();
                group_prompt.add(&entry_pwd);

                let switch_save = adw::SwitchRow::builder()
                    .title("Passwort im Schlüsselbund speichern")
                    .active(row_remember_password_ref.is_active())
                    .build();
                group_prompt.add(&switch_save);

                content_box.append(&group_prompt);
                alert.set_extra_child(Some(&content_box));

                alert.add_response("cancel", "Abbrechen");
                alert.add_response("confirm", "Anmelden");
                alert.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
                alert.set_default_response(Some("confirm"));
                alert.set_close_response("cancel");

                let row_email_ref = row_email_ref.clone();
                let row_username_ref = row_username_ref.clone();
                let row_password_ref = row_password_ref.clone();
                let row_remember_password_ref = row_remember_password_ref.clone();
                let row_fetch_url_ref = row_fetch_url_ref.clone();
                let row_imap_host_ref = row_imap_host_ref.clone();
                let row_oab_url_ref = row_oab_url_ref.clone();
                let row_open_other_mailbox_ref = row_open_other_mailbox_ref.clone();
                let row_other_mailbox_user_ref = row_other_mailbox_user_ref.clone();
                let row_check_auth_ref = row_check_auth_ref.clone();
                let row_auth_combo_ref = row_auth_combo_ref.clone();
                let btn_fetch_ref = btn_fetch_ref.clone();
                let dialog_weak_inner = dialog.downgrade();
                let entry_user_clone = entry_user.clone();
                let entry_pwd_clone = entry_pwd.clone();
                let switch_save_clone = switch_save.clone();

                alert.connect_response(None, move |_, response| {
                    if response != "confirm" {
                        return;
                    }

                    let user = entry_user_clone.text().trim().to_string();
                    let pwd = entry_pwd_clone.text().to_string();
                    let remember = switch_save_clone.is_active();

                    row_username_ref.set_text(&user);
                    row_password_ref.set_text(&pwd);
                    row_remember_password_ref.set_active(remember);

                    let email_for_thread = row_email_ref.text().trim().to_string();

                    // Auto-detect shared mailbox if username differs from mailbox email
                    if !user.is_empty()
                        && !email_for_thread.is_empty()
                        && user.to_lowercase() != email_for_thread.to_lowercase()
                    {
                        row_open_other_mailbox_ref.set_active(true);
                        row_other_mailbox_user_ref.set_sensitive(true);
                        if row_other_mailbox_user_ref.text().trim().is_empty() {
                            row_other_mailbox_user_ref.set_text(&email_for_thread);
                        }
                    }

                    if user.is_empty() || pwd.is_empty() {
                        row_fetch_url_ref.set_subtitle("⚠️ Benutzername und Passwort dürfen nicht leer sein.");
                        if let Some(dlg) = dialog_weak_inner.upgrade() {
                            dlg.add_toast(&adw::Toast::new("Bitte Benutzername und Passwort eingeben"));
                        }
                        return;
                    }

                    btn_fetch_ref.set_sensitive(false);
                    let input_host = row_imap_host_ref.text().trim().to_string();
                    let email_for_thread = row_email_ref.text().trim().to_string();

                    if input_host.is_empty() {
                        row_fetch_url_ref.set_subtitle("Exchange-Dienste werden via Autodiscover ermittelt …");
                    } else {
                        row_fetch_url_ref.set_subtitle("Exchange-Dienste werden geprüft …");
                    }

                    let (tx, rx) = async_channel::bounded::<crate::services::account_config_service::ExchangeDiscoveryStatus>(1);
                    std::thread::spawn(move || {
                        let result = crate::services::account_config_service::discover_and_validate_exchange_services(
                            &email_for_thread,
                            &input_host,
                            &user,
                            &pwd,
                        );
                        let _ = tx.send_blocking(result);
                    });

                    let row_fetch_clone = row_fetch_url_ref.clone();
                    let row_imap_host_clone = row_imap_host_ref.clone();
                    let row_oab_url_clone = row_oab_url_ref.clone();
                    let row_check_auth_clone = row_check_auth_ref.clone();
                    let row_auth_combo_clone = row_auth_combo_ref.clone();
                    let btn_fetch_clone = btn_fetch_ref.clone();
                    let dialog_weak_async = dialog_weak_inner.clone();

                    glib::spawn_future_local(async move {
                        btn_fetch_clone.set_sensitive(true);
                        if let Ok(res) = rx.recv().await {
                            match res {
                                crate::services::account_config_service::ExchangeDiscoveryStatus::Success { ews_url, oab_url, message } => {
                                    row_imap_host_clone.set_text(&ews_url);
                                    row_oab_url_clone.set_text(&oab_url);
                                    row_fetch_clone.set_subtitle(&message);
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new("Exchange-Endpunkte erfolgreich ermittelt"));
                                    }

                                    // Automatically query supported auth methods and select best
                                    let host_for_auth = ews_url.clone();
                                    let row_check_auth_async = row_check_auth_clone.clone();
                                    let row_auth_combo_async = row_auth_combo_clone.clone();
                                    let (tx_auth, rx_auth) = async_channel::bounded::<Result<Vec<String>, String>>(1);
                                    std::thread::spawn(move || {
                                        let res = crate::services::account_config_service::query_exchange_auth_methods(&host_for_auth);
                                        let _ = tx_auth.send_blocking(res);
                                    });
                                    glib::spawn_future_local(async move {
                                        if let Ok(Ok(methods)) = rx_auth.recv().await {
                                            let summary = methods.join(", ");
                                            let mut selected_name = "";
                                            if methods.iter().any(|m| m.contains("NTLM")) {
                                                row_auth_combo_async.set_selected(0);
                                                selected_name = "NTLM";
                                            } else if methods.iter().any(|m| m.contains("Kerberos")) {
                                                row_auth_combo_async.set_selected(2);
                                                selected_name = "Kerberos";
                                            } else if methods.iter().any(|m| m.contains("OAuth2")) {
                                                row_auth_combo_async.set_selected(3);
                                                selected_name = "OAuth2";
                                            } else if methods.iter().any(|m| m.contains("Basic")) {
                                                row_auth_combo_async.set_selected(1);
                                                selected_name = "Basic";
                                            }
                                            row_check_auth_async.set_subtitle(&format!("✓ Unterstützt: {summary} ({selected_name} gewählt)"));
                                        }
                                    });
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::MissingCredentials => {
                                    row_fetch_clone.set_subtitle("⚠️ Bitte Benutzername und Passwort eingeben.");
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::AuthFailed(msg) => {
                                    row_fetch_clone.set_subtitle(&format!("❌ {msg}"));
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new("Authentifizierung fehlgeschlagen: Bitte Passwort prüfen"));
                                    }
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::ServerNotFound(msg) => {
                                    row_fetch_clone.set_subtitle(&format!("❌ {msg}"));
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new(&msg));
                                    }
                                    row_imap_host_clone.grab_focus();
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::Error(msg) => {
                                    row_fetch_clone.set_subtitle(&format!("❌ {msg}"));
                                }
                            }
                        }
                    });
                });

                alert.present(Some(&dialog));
                entry_pwd.grab_focus();
            });
        }

        // 1e. Sending (SMTP)
        let group_smtp = adw::PreferencesGroup::builder()
            .title("Postausgangsserver (SMTP)")
            .description("Server für ausgehende E-Mails")
            .build();

        let row_smtp_exchange_info = adw::ActionRow::builder()
            .title("E-Mail-Versand über Exchange")
            .subtitle("Der E-Mail-Versand erfolgt direkt und authentifiziert über die Exchange-/EWS-Verbindung.")
            .build();
        let icon_check = gtk::Image::from_icon_name("emblem-default-symbolic");
        row_smtp_exchange_info.add_prefix(&icon_check);
        row_smtp_exchange_info.set_visible(false);
        group_smtp.add(&row_smtp_exchange_info);

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

        // 1f. Check & Sync Options
        let group_sync = adw::PreferencesGroup::builder()
            .title("Abruf- und Bereinigungsoptionen")
            .description("Einstellungen für die automatische Postfachsynchronisation")
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

        // 1g. Default folders & composing
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

        // 1h. Security (PGP / S/MIME)
        let group_pgp = adw::PreferencesGroup::builder()
            .title("OpenPGP-Verschlüsselung und Signaturen")
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

        // ─────────────────────────────────────────────────────────────────────
        // ── 2. PROTOCOL FIELDS DYNAMICS ───────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let update_protocol_fields = {
            let row_imap_host = row_imap_host.clone();
            let row_fetch_url = row_fetch_url.clone();
            let row_oab_url = row_oab_url.clone();
            let row_open_other_mailbox = row_open_other_mailbox.clone();
            let row_other_mailbox_user = row_other_mailbox_user.clone();
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
            let row_auth_combo = row_auth_combo.clone();
            let row_check_auth = row_check_auth.clone();
            let row_password = row_password.clone();
            let row_smtp_exchange_info = row_smtp_exchange_info.clone();
            let row_smtp_auth_info = row_smtp_auth_info.clone();
            let group_smtp = group_smtp.clone();

            move |selected: u32| {
                let is_exchange = selected == 1;
                let is_goa = selected == 2;
                let is_pop3 = selected == 3;
                let has_manual_servers = !is_goa;
                let has_manual_smtp = !is_goa && !is_exchange;

                row_imap_host.set_sensitive(has_manual_servers);
                row_imap_host.set_title(if is_exchange {
                    "Rechneradresse (Host-URL)"
                } else if is_pop3 {
                    "Posteingangsserver (POP3)"
                } else if is_goa {
                    "Posteingangsserver (über Onlinekonto verwaltet)"
                } else {
                    "Posteingangsserver (IMAP)"
                });

                if is_exchange {
                    row_imap_host.set_tooltip_text(Some("Adresse des Exchange-Servers (z. B. mail.firma.de oder https://mail.firma.de/EWS/Exchange.asmx)"));
                } else {
                    row_imap_host.set_tooltip_text(None);
                }

                row_fetch_url.set_visible(is_exchange);
                row_oab_url.set_visible(is_exchange);
                row_open_other_mailbox.set_visible(is_exchange);
                row_other_mailbox_user.set_visible(is_exchange);
                row_check_auth.set_visible(is_exchange);
                row_check_auth.set_sensitive(!is_goa);
                row_imap_port.set_visible(!is_exchange && has_manual_servers);
                row_imap_port.set_sensitive(has_manual_servers);

                row_smtp_exchange_info.set_visible(is_exchange);
                row_smtp_host.set_visible(has_manual_smtp);
                row_smtp_port.set_visible(has_manual_smtp);
                row_smtp_auth_info.set_visible(has_manual_smtp);

                if is_goa && cfg!(target_os = "macos") {
                    group_smtp.set_description(Some(
                        "macOS Internetaccounts stellt Kalender, Erinnerungen und Kontakte als Systemquellen bereit. E-Mail benötigt eine eigene Anmeldung in dieser App.",
                    ));
                } else if is_goa {
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
                row_auth_combo.set_sensitive(!is_goa);
                row_password.set_sensitive(!is_goa);

                if is_exchange {
                    row_username.set_tooltip_text(Some(
                        "z. B. DOMÄNE\\Benutzername oder E-Mail-Adresse",
                    ));
                } else if is_goa && cfg!(target_os = "macos") {
                    row_username.set_tooltip_text(Some(
                        "macOS Internetaccounts übergibt keine allgemeinen Mail-Zugangsdaten; für E-Mail bitte ein eigenes Konto hinzufügen",
                    ));
                } else if is_goa {
                    row_username.set_tooltip_text(Some(
                        "Das Onlinekonto verwaltet Benutzername und Zugangsdaten",
                    ));
                } else {
                    row_username.set_tooltip_text(None);
                }
            }
        };

        update_protocol_fields(combo_type.selected());
        {
            let update_clone = update_protocol_fields.clone();
            combo_type.connect_selected_notify(move |combo| {
                update_clone(combo.selected());
            });
        }

        // ─────────────────────────────────────────────────────────────────────
        // ── AUTODISCOVER SERVICE & DEBOUNCING WIRING ─────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        if let Some(ref profile) = edit_profile {
            self.imp().last_discovered_email.replace(profile.email.clone());
        }

        let run_discovery = {
            let dialog_weak = self.downgrade();
            let row_imap_host = row_imap_host.clone();
            let row_imap_port = row_imap_port.clone();
            let row_smtp_host = row_smtp_host.clone();
            let row_smtp_port = row_smtp_port.clone();
            let row_security = row_security.clone();
            let row_username = row_username.clone();
            let row_fullname = row_fullname.clone();
            let combo_type = combo_type.clone();
            let discovery_spinner = discovery_spinner.clone();
            let row_discovery_status = row_discovery_status.clone();
            let icon_discovery = icon_discovery.clone();
            let btn_redetect = btn_redetect.clone();

            move |email: String, generation: u64| {
                let Some(dialog) = dialog_weak.upgrade() else { return; };
                if dialog.imp().discovery_generation.get() != generation {
                    return;
                }

                let domain = email.split_once('@').map(|(_, d)| d).unwrap_or("").to_string();
                dialog.imp().last_discovered_email.replace(email.clone());

                // UI feedback: in progress
                discovery_spinner.set_visible(true);
                discovery_spinner.start();
                row_discovery_status.set_visible(true);
                row_discovery_status.set_title("Suche Servereinstellungen…");
                row_discovery_status.set_subtitle(&format!("Einstellungen für „{domain}“ werden gesucht…"));
                icon_discovery.set_icon_name(Some("network-transmit-receive-symbolic"));
                icon_discovery.remove_css_class("accent");
                btn_redetect.set_visible(false);

                // Setup cancellation
                let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                dialog.imp().discovery_cancellation.replace(Some(cancellation.clone()));

                let (tx, rx) = async_channel::bounded::<(u64, Result<crate::services::autodiscover_service::DiscoveredAccountConfig, String>)>(1);
                let email_for_worker = email.clone();
                let cancellation_for_worker = cancellation.clone();

                std::thread::spawn(move || {
                    if cancellation_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let result = crate::services::autodiscover_service::AutodiscoverService::discover_cancelable(
                        &email_for_worker,
                        &cancellation_for_worker,
                    );
                    if cancellation_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let _ = tx.send_blocking((generation, result));
                });

                let dialog_weak_inner = dialog.downgrade();
                let row_imap_host_clone = row_imap_host.clone();
                let row_imap_port_clone = row_imap_port.clone();
                let row_smtp_host_clone = row_smtp_host.clone();
                let row_smtp_port_clone = row_smtp_port.clone();
                let row_security_clone = row_security.clone();
                let row_username_clone = row_username.clone();
                let row_fullname_clone = row_fullname.clone();
                let combo_type_clone = combo_type.clone();
                let discovery_spinner_clone = discovery_spinner.clone();
                let row_discovery_status_clone = row_discovery_status.clone();
                let icon_discovery_clone = icon_discovery.clone();
                let btn_redetect_clone = btn_redetect.clone();

                glib::spawn_future_local(async move {
                    if let Ok((res_gen, result)) = rx.recv().await {
                        let Some(dialog) = dialog_weak_inner.upgrade() else { return; };
                        if dialog.imp().discovery_generation.get() != res_gen {
                            return; // Obsolete generation dropped
                        }

                        discovery_spinner_clone.stop();
                        discovery_spinner_clone.set_visible(false);
                        btn_redetect_clone.set_visible(true);

                        match result {
                            Ok(config) => {
                                // Auto-fill incoming server
                                row_imap_host_clone.set_text(config.imap_host());
                                row_imap_port_clone.set_text(&config.imap_port().to_string());
                                row_security_clone.set_selected(config.incoming_tls_mode().row_security_index());

                                // Auto-fill outgoing server
                                row_smtp_host_clone.set_text(config.smtp_host());
                                row_smtp_port_clone.set_text(&config.smtp_port().to_string());

                                // Auto-fill username with template substitution
                                row_username_clone.set_text(&config.resolved_username());

                                // Suggest display name if empty
                                if row_fullname_clone.text().trim().is_empty() && !config.display_name.is_empty() {
                                    row_fullname_clone.set_text(&config.display_name);
                                }

                                // Ensure standard IMAP/SMTP protocol selected
                                if combo_type_clone.selected() != 0 {
                                    combo_type_clone.set_selected(0);
                                }

                                // Status feedback
                                row_discovery_status_clone.set_visible(true);
                                row_discovery_status_clone.set_title("Servereinstellungen gefunden");
                                row_discovery_status_clone.set_subtitle(&format!(
                                    "✓ Konfiguration für {} automatisch übernommen (IMAP: {}:{}, SMTP: {}:{})",
                                    config.display_name,
                                    config.imap_host(),
                                    config.imap_port(),
                                    config.smtp_host(),
                                    config.smtp_port()
                                ));
                                icon_discovery_clone.set_icon_name(Some("emblem-ok-symbolic"));
                                icon_discovery_clone.add_css_class("accent");
                            }
                            Err(_err) => {
                                // Graceful fallback to heuristic defaults for empty fields
                                if row_imap_host_clone.text().trim().is_empty() {
                                    row_imap_host_clone.set_text(&format!("imap.{domain}"));
                                    row_imap_port_clone.set_text("993");
                                    row_security_clone.set_selected(0);
                                }
                                if row_smtp_host_clone.text().trim().is_empty() {
                                    row_smtp_host_clone.set_text(&format!("smtp.{domain}"));
                                    row_smtp_port_clone.set_text("587");
                                }
                                if row_username_clone.text().trim().is_empty() {
                                    row_username_clone.set_text(&email);
                                }

                                row_discovery_status_clone.set_visible(true);
                                row_discovery_status_clone.set_title("Standardwerte vorgeschlagen");
                                row_discovery_status_clone.set_subtitle(&format!(
                                    "Servereinstellungen für „{domain}“ konnten nicht automatisch ermittelt werden. Standardwerte wurden vorgeschlagen — bitte manuell prüfen."
                                ));
                                icon_discovery_clone.set_icon_name(Some("dialog-information-symbolic"));
                                icon_discovery_clone.remove_css_class("accent");

                                dialog.add_toast(&adw::Toast::new(
                                    &format!("Servereinstellungen für {domain} nicht ermittelt — Standardwerte vorgeschlagen"),
                                ));
                            }
                        }
                    }
                });
            }
        };

        // Wire redetect button
        {
            let run_discovery = run_discovery.clone();
            let row_email_clone = row_email.clone();
            let dialog_weak = self.downgrade();
            btn_redetect.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else { return; };
                let text = row_email_clone.text().trim().to_string();
                if let Some((_user, _domain)) = is_discovery_candidate(&text) {
                    let generation_id = dialog.imp().discovery_generation.get().wrapping_add(1);
                    dialog.imp().discovery_generation.set(generation_id);
                    run_discovery(text, generation_id);
                }
            });
        }

        // Wire row_email changed signal with debouncing
        let dialog_weak = self.downgrade();
        let run_discovery_clone = run_discovery.clone();
        let discovery_spinner_clone = discovery_spinner.clone();
        let row_discovery_status_clone = row_discovery_status.clone();
        row_email.connect_changed(move |entry| {
            let t = entry.text();

            let Some(dialog) = dialog_weak.upgrade() else { return; };
            let raw = t.trim().to_string();

            // Cancel any active debounce source
            if let Some(source) = dialog.imp().debounce_source.borrow_mut().take() {
                source.remove();
            }

            // If empty or invalid, cancel active query and reset status
            let Some((_user, _domain)) = is_discovery_candidate(&raw) else {
                if let Some(cancellation) = dialog.imp().discovery_cancellation.borrow().as_ref() {
                    cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                discovery_spinner_clone.stop();
                discovery_spinner_clone.set_visible(false);
                row_discovery_status_clone.set_visible(false);
                return;
            };

            // Avoid redundant query if already discovered for this email
            if *dialog.imp().last_discovered_email.borrow() == raw {
                return;
            }

            // Cancel previous background worker
            if let Some(cancellation) = dialog.imp().discovery_cancellation.borrow().as_ref() {
                cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            let generation_id = dialog.imp().discovery_generation.get().wrapping_add(1);
            dialog.imp().discovery_generation.set(generation_id);

            let run_discovery = run_discovery_clone.clone();
            let dialog_weak_timer = dialog.downgrade();
            let source = glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
                let Some(dialog) = dialog_weak_timer.upgrade() else { return; };
                dialog.imp().debounce_source.borrow_mut().take();
                run_discovery(raw, generation_id);
            });
            dialog.imp().debounce_source.replace(Some(source));
        });

        // Wire entry activate (Enter key) for immediate non-debounced discovery
        let dialog_weak = self.downgrade();
        let run_discovery_clone2 = run_discovery.clone();
        row_email.connect_entry_activated(move |entry| {
            let Some(dialog) = dialog_weak.upgrade() else { return; };
            let raw = entry.text().trim().to_string();
            if let Some((_user, _domain)) = is_discovery_candidate(&raw) {
                if *dialog.imp().last_discovered_email.borrow() != raw {
                    if let Some(source) = dialog.imp().debounce_source.borrow_mut().take() {
                        source.remove();
                    }
                    let generation_id = dialog.imp().discovery_generation.get().wrapping_add(1);
                    dialog.imp().discovery_generation.set(generation_id);
                    run_discovery_clone2(raw, generation_id);
                }
            }
        });

        // ── 3. SAVE ROUTINE ──────────────────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let edit_id = edit_profile.as_ref().map(|profile| profile.id.clone());
        let edit_enabled = edit_profile
            .as_ref()
            .map(|profile| profile.enabled)
            .unwrap_or(true);
        let edit_is_shared = edit_profile
            .as_ref()
            .map(|profile| profile.is_shared_mailbox)
            .unwrap_or(false);
        let edit_parent_account_id = edit_profile
            .as_ref()
            .and_then(|profile| profile.parent_account_id.clone());

        let do_save = {
            let dialog = self.downgrade();
            let combo_type = combo_type.clone();
            let row_fullname = row_fullname.clone();
            let row_email = row_email.clone();
            let row_reply_to = row_reply_to.clone();
            let row_organization = row_organization.clone();
            let row_aliases = row_aliases.clone();
            let row_signature = row_signature.clone();
            let row_imap_host = row_imap_host.clone();
            let row_oab_url = row_oab_url.clone();
            let row_open_other_mailbox = row_open_other_mailbox.clone();
            let row_other_mailbox_user = row_other_mailbox_user.clone();
            let row_smtp_host = row_smtp_host.clone();
            let row_imap_port = row_imap_port.clone();
            let row_smtp_port = row_smtp_port.clone();
            let row_auto_check = row_auto_check.clone();
            let row_check_interval = row_check_interval.clone();
            let row_check_all_folders = row_check_all_folders.clone();
            let row_expunge_on_exit = row_expunge_on_exit.clone();
            let row_username = row_username.clone();
            let row_auth_combo = row_auth_combo.clone();
            let row_password = row_password.clone();
            let row_remember_password = row_remember_password.clone();
            let row_security = row_security.clone();
            let row_inbox_folder = row_inbox_folder.clone();
            let row_drafts_folder = row_drafts_folder.clone();
            let row_sent_folder = row_sent_folder.clone();
            let row_archive_folder = row_archive_folder.clone();
            let row_trash_folder = row_trash_folder.clone();
            let row_junk_folder = row_junk_folder.clone();
            let row_pgp_key = row_pgp_key.clone();
            let row_pgp_sign = row_pgp_sign.clone();
            let row_pgp_encrypt = row_pgp_encrypt.clone();
            let row_smime_sign = row_smime_sign.clone();
            let row_smime_encrypt = row_smime_encrypt.clone();
            let row_prefer_html = row_prefer_html.clone();
            let row_default_cc = row_default_cc.clone();
            let row_default_bcc = row_default_bcc.clone();

            move || -> bool {
                let Some(dialog) = dialog.upgrade() else {
                    return false;
                };

                #[cfg(target_os = "macos")]
                if combo_type.selected() == 2 {
                    dialog.add_toast(&adw::Toast::new(
                        "macOS Internetaccounts verwaltet nur native Kalender-, Erinnerungs- und Kontaktquellen. Für E-Mail bitte IMAP, Exchange oder POP3 einrichten.",
                    ));
                    return false;
                }
                let mut email = row_email.text().trim().to_string();
                let mut display_name = row_fullname.text().trim().to_string();
                let username = row_username.text().trim().to_string();

                if email.is_empty() && username.contains('@') {
                    email = username.clone();
                    row_email.set_text(&email);
                }

                if email.is_empty() || !crate::services::account_config_service::is_plausible_address(&email) {
                    dialog.add_toast(&adw::Toast::new("Bitte eine gültige E-Mail-Adresse eingeben"));
                    row_email.grab_focus();
                    return false;
                }

                let protocol = match combo_type.selected() {
                    1 => AccountProtocol::Exchange,
                    2 => AccountProtocol::Goa,
                    3 => AccountProtocol::Pop3,
                    _ => AccountProtocol::ImapSmtp,
                };

                if display_name.is_empty() {
                    display_name = if let Some((user, _)) = email.split_once('@') {
                        user.to_string()
                    } else {
                        email.clone()
                    };
                    row_fullname.set_text(&display_name);
                }

                let mut smtp_host = row_smtp_host.text().trim().to_string();
                let imap_host = row_imap_host.text().trim().to_string();
                if smtp_host.is_empty() && !imap_host.is_empty() && protocol == AccountProtocol::ImapSmtp {
                    smtp_host = if let Some(rest) = imap_host.strip_prefix("imap.") {
                        format!("smtp.{rest}")
                    } else {
                        imap_host.clone()
                    };
                    row_smtp_host.set_text(&smtp_host);
                }

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

                let auth_method = match row_auth_combo.selected() {
                    1 => "Basic".to_string(),
                    2 => "GSSAPI".to_string(),
                    3 => "OAuth2".to_string(),
                    _ => "NTLM".to_string(),
                };

                let open_mailbox_other_user = row_open_other_mailbox.is_active();
                let foreign_mailbox = if open_mailbox_other_user {
                    let entered = row_other_mailbox_user.text().trim().to_string();
                    if entered.is_empty() {
                        email.clone()
                    } else {
                        entered
                    }
                } else {
                    String::new()
                };

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
                    oab_url: crate::services::account_config_service::sanitize_oab_url(&row_oab_url.text()),
                    imap_port,
                    smtp_host: row_smtp_host.text().trim().to_string(),
                    smtp_port,
                    username: row_username.text().trim().to_string(),
                    auth_method,
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
                    open_mailbox_other_user,
                    foreign_mailbox,
                    is_shared_mailbox: open_mailbox_other_user || edit_is_shared,
                    parent_account_id: edit_parent_account_id.clone(),
                };

                if let Err(error) = profile.validate() {
                    dialog.add_toast(&adw::Toast::new(&error));
                    return false;
                }

                let profile_id = profile.id.clone();
                let password = row_password.text().to_string();
                let remember_password = row_remember_password.is_active();

                match AccountConfigStore::default().upsert(profile.clone()) {
                    Ok(()) => {
                        if let Some(settings) = AppSettings::try_new() {
                            if settings.default_account_id().trim().is_empty() {
                                settings.set_default_account_id(&profile_id);
                            }
                        }

                        let pwd_opt = if remember_password && !password.is_empty() {
                            Some(password.as_str())
                        } else {
                            None
                        };
                        let _ = crate::services::account_config_service::sync_profile_to_eds(&profile, pwd_opt);

                        if let Some(window) = crate::window::find_evolution_window(&dialog) {
                            window.reload_accounts();
                            window.add_toast(&adw::Toast::new(if is_editing {
                                "Kontoeinstellungen gespeichert"
                            } else {
                                "Konto erfolgreich eingerichtet"
                            }));
                        }
                        dialog.close();
                        true
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist account profile");
                        dialog.add_toast(&adw::Toast::new(&format!(
                            "Kontoprofil konnte nicht gespeichert werden: {error}"
                        )));
                        false
                    }
                }
            }
        };

        // ─────────────────────────────────────────────────────────────────────
        // ── 4. UNIFIED 2-COLUMN SIDEBAR LAYOUT (Setup & Edit) ────────────────
        // ─────────────────────────────────────────────────────────────────────
        let toast_overlay = adw::ToastOverlay::new();
        self.imp().toast_overlay.replace(Some(toast_overlay.clone()));

        let split_view = adw::NavigationSplitView::builder()
            .min_sidebar_width(280.0)
            .max_sidebar_width(340.0)
            .sidebar_width_fraction(0.32)
            .build();
        self.imp().split_view.replace(Some(split_view.clone()));

        let btn_cancel = gtk::Button::builder().label("Abbrechen").build();
        btn_cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));

        let sidebar_toolbar_view = adw::ToolbarView::new();
        let sidebar_header_bar = adw::HeaderBar::new();
        sidebar_header_bar.set_show_title(false);
        sidebar_header_bar.set_show_start_title_buttons(false);
        sidebar_header_bar.set_show_end_title_buttons(false);
        sidebar_header_bar.pack_start(&btn_cancel);
        sidebar_toolbar_view.add_top_bar(&sidebar_header_bar);

        let window_title_str = if let Some(ref profile) = edit_profile {
            if !profile.display_name.is_empty() {
                format!("Kontoeinstellungen — {}", profile.display_name)
            } else {
                format!("Kontoeinstellungen — {}", profile.email)
            }
        } else {
            "Neues Konto einrichten".to_string()
        };

        let (initial_name, initial_email, initial_badge, status_text) = if let Some(ref profile) = edit_profile {
            let name = if !profile.display_name.is_empty() {
                profile.display_name.clone()
            } else if !profile.email.is_empty() {
                profile.email.clone()
            } else {
                "Konto".to_string()
            };
            let email = if !profile.email.is_empty() {
                profile.email.clone()
            } else {
                "Keine E-Mail-Adresse".to_string()
            };
            let badge = match profile.protocol {
                AccountProtocol::Exchange => "Microsoft Exchange (EWS)",
                AccountProtocol::ImapSmtp => "IMAP / SMTP",
                AccountProtocol::Pop3 => "POP3 / SMTP",
                AccountProtocol::Goa => "GNOME-Onlinekonto",
            };
            (name, email, badge.to_string(), "Konto aktiv".to_string())
        } else {
            (
                "Neues Konto".to_string(),
                "Wird eingerichtet …".to_string(),
                "IMAP / SMTP".to_string(),
                "Schritt 1 von 5".to_string(),
            )
        };

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(12)
            .build();

        let profile_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(6)
            .build();

        let avatar = adw::Avatar::builder()
            .size(54)
            .text(&initial_name)
            .show_initials(true)
            .build();
        profile_box.append(&avatar);

        let lbl_name = gtk::Label::builder()
            .label(&initial_name)
            .css_classes(vec!["title-3"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Center)
            .build();
        profile_box.append(&lbl_name);

        let lbl_email = gtk::Label::builder()
            .label(&initial_email)
            .css_classes(vec!["caption", "dim-label"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .halign(gtk::Align::Center)
            .build();
        profile_box.append(&lbl_email);

        let badge_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::Center)
            .margin_top(2)
            .build();
        let lbl_badge = gtk::Label::builder()
            .label(&initial_badge)
            .css_classes(vec!["caption", "accent"])
            .halign(gtk::Align::Center)
            .build();
        badge_box.append(&lbl_badge);
        profile_box.append(&badge_box);

        sidebar_box.append(&profile_box);

        // In setup mode, update sidebar avatar/name/email in real time as user types
        if !is_editing {
            let lbl_name_clone = lbl_name.clone();
            let avatar_clone = avatar.clone();
            row_fullname.connect_notify_local(Some("text"), move |entry, _| {
                let name = entry.text().trim().to_string();
                if name.is_empty() {
                    lbl_name_clone.set_label("Neues Konto");
                    avatar_clone.set_text(Some("Neues Konto"));
                } else {
                    lbl_name_clone.set_label(&name);
                    avatar_clone.set_text(Some(&name));
                }
            });

            let lbl_email_clone = lbl_email.clone();
            row_email.connect_notify_local(Some("text"), move |entry, _| {
                let email = entry.text().trim().to_string();
                if email.is_empty() {
                    lbl_email_clone.set_label("Wird eingerichtet …");
                } else {
                    lbl_email_clone.set_label(&email);
                }
            });
        }

        let sep = gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&sep);

        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        fn make_sidebar_row(icon_name: &str, title: &str, subtitle: &str, page_name: &str) -> gtk::ListBoxRow {
            let row = gtk::ListBoxRow::builder()
                .activatable(true)
                .selectable(true)
                .css_classes(vec!["sidebar-item"])
                .build();
            let hbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(12)
                .margin_start(10)
                .margin_end(10)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            let icon = gtk::Image::builder()
                .icon_name(icon_name)
                .pixel_size(20)
                .valign(gtk::Align::Center)
                .build();
            let vbox = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            let label = gtk::Label::builder()
                .label(title)
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading"])
                .build();
            let sub = gtk::Label::builder()
                .label(subtitle)
                .halign(gtk::Align::Start)
                .css_classes(vec!["caption", "dim-label"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            vbox.append(&label);
            vbox.append(&sub);
            hbox.append(&icon);
            hbox.append(&vbox);
            row.set_child(Some(&hbox));
            unsafe {
                row.set_data("page-name", page_name.to_string());
                row.set_data("page-title", title.to_string());
            }
            row
        }

        let row_nav_identity = make_sidebar_row("avatar-default-symbolic", "Identität", "Name, Adresse & Signatur", "identity");
        let row_nav_receiving = make_sidebar_row("mail-unread-symbolic", "Empfangen", "Server, EWS & Anmeldedaten", "receiving");
        let row_nav_sending = make_sidebar_row("mail-send-symbolic", "Versenden", "Postausgang & Übermittlung", "sending");
        let row_nav_defaults = make_sidebar_row("folder-symbolic", "Standardordner", "Entwürfe, Archiv & Optionen", "defaults");
        let row_nav_security = make_sidebar_row("security-high-symbolic", "Sicherheit", "OpenPGP & Abschluss", "security");

        sidebar_list.append(&row_nav_identity);
        sidebar_list.append(&row_nav_receiving);
        sidebar_list.append(&row_nav_sending);
        sidebar_list.append(&row_nav_defaults);
        sidebar_list.append(&row_nav_security);
        sidebar_list.select_row(Some(&row_nav_identity));

        sidebar_box.append(&sidebar_list);

        let spacer = gtk::Box::builder().vexpand(true).build();
        sidebar_box.append(&spacer);

        let status_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_bottom(12)
            .margin_top(4)
            .build();
        let dot = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .pixel_size(14)
            .css_classes(vec!["accent"])
            .build();
        let lbl_status = gtk::Label::builder()
            .label(&status_text)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        status_box.append(&dot);
        status_box.append(&lbl_status);
        sidebar_box.append(&status_box);

        let sidebar_scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .css_classes(vec!["sidebar"])
            .child(&sidebar_box)
            .build();
        sidebar_toolbar_view.set_content(Some(&sidebar_scrolled));

        let sidebar_nav_page = adw::NavigationPage::new(&sidebar_toolbar_view, "Seitenleiste");
        split_view.set_sidebar(Some(&sidebar_nav_page));

        let content_toolbar_view = adw::ToolbarView::new();
        let content_header_bar = adw::HeaderBar::new();
        // Remove close 'x' buttons from header bar
        content_header_bar.set_show_start_title_buttons(false);
        content_header_bar.set_show_end_title_buttons(false);

        let initial_subtitle = if is_editing {
            "Identität".to_string()
        } else {
            "Schritt 1 von 5: Identität".to_string()
        };
        let window_title = adw::WindowTitle::builder()
            .title(&window_title_str)
            .subtitle(&initial_subtitle)
            .build();
        content_header_bar.set_title_widget(Some(&window_title));

        let btn_finish = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();

        let btn_back = gtk::Button::builder()
            .label("Zurück")
            .icon_name("go-previous-symbolic")
            .build();
        btn_back.set_visible(false);

        let btn_next = gtk::Button::builder()
            .label("Weiter")
            .css_classes(vec!["suggested-action"])
            .build();

        if is_editing {
            content_header_bar.pack_end(&btn_finish);
        } else {
            content_header_bar.pack_start(&btn_back);
            content_header_bar.pack_end(&btn_next);
        }
        content_toolbar_view.add_top_bar(&content_header_bar);

        let view_stack = adw::ViewStack::builder()
            .vexpand(true)
            .hexpand(true)
            .build();
        content_toolbar_view.set_content(Some(&view_stack));

        let content_nav_page = adw::NavigationPage::new(&content_toolbar_view, "Einstellungen");
        split_view.set_content(Some(&content_nav_page));

        self.imp().sidebar_list.replace(Some(sidebar_list.clone()));
        self.imp().view_stack.replace(Some(view_stack.clone()));

        let bp_narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 620sp").unwrap());
        bp_narrow.add_setter(&split_view, "collapsed", Some(&true.to_value()));
        self.add_breakpoint(bp_narrow);

        let page_identity = adw::PreferencesPage::builder()
            .title("Identität")
            .icon_name("avatar-default-symbolic")
            .build();
        if !is_editing {
            let group_wizard_intro = adw::PreferencesGroup::builder()
                .title("Willkommen beim Evolution-Einrichtungsassistenten")
                .description("Dieser Assistent führt Sie Schritt für Schritt durch die Einrichtung Ihres E-Mail-Kontos. Bitte wählen Sie die Kontoart und geben Sie Ihren Namen und Ihre E-Mail-Adresse an.")
                .build();
            page_identity.add(&group_wizard_intro);
        }
        page_identity.add(&group_type);
        page_identity.add(&group_user);

        let page_receiving = adw::PreferencesPage::builder()
            .title("Empfangen")
            .icon_name("mail-unread-symbolic")
            .build();
        page_receiving.add(&group_server);
        page_receiving.add(&group_cred);
        page_receiving.add(&group_sync);

        let page_sending = adw::PreferencesPage::builder()
            .title("Versenden")
            .icon_name("mail-send-symbolic")
            .build();
        page_sending.add(&group_smtp);

        let page_defaults = adw::PreferencesPage::builder()
            .title("Standardordner")
            .icon_name("folder-symbolic")
            .build();
        page_defaults.add(&group_folders);
        page_defaults.add(&group_composing);

        let page_security = adw::PreferencesPage::builder()
            .title("Sicherheit")
            .icon_name("security-high-symbolic")
            .build();
        page_security.add(&group_pgp);
        page_security.add(&group_smime);
        if !is_editing {
            let group_wizard_finish = adw::PreferencesGroup::builder()
                .title("Zusammenfassung & Abschluss")
                .description("Alle Einstellungen sind bereit. Klicken Sie auf »Konto einrichten«, um die Konfiguration zu speichern und das Konto zu aktivieren.")
                .build();
            page_security.add(&group_wizard_finish);
        }

        view_stack.add_titled_with_icon(&page_identity, Some("identity"), "Identität", "avatar-default-symbolic");
        view_stack.add_titled_with_icon(&page_receiving, Some("receiving"), "Empfangen", "mail-unread-symbolic");
        view_stack.add_titled_with_icon(&page_sending, Some("sending"), "Versenden", "mail-send-symbolic");
        view_stack.add_titled_with_icon(&page_defaults, Some("defaults"), "Standardordner", "folder-symbolic");
        view_stack.add_titled_with_icon(&page_security, Some("security"), "Sicherheit", "security-high-symbolic");

        let validate_step = {
            let dialog_weak = self.downgrade();
            let combo_type = combo_type.clone();
            let row_fullname = row_fullname.clone();
            let row_email = row_email.clone();
            let row_imap_host = row_imap_host.clone();
            let row_open_other_mailbox = row_open_other_mailbox.clone();
            let row_other_mailbox_user = row_other_mailbox_user.clone();
            let row_username = row_username.clone();
            let row_password = row_password.clone();
            let row_imap_port = row_imap_port.clone();
            let row_smtp_host = row_smtp_host.clone();
            let row_smtp_port = row_smtp_port.clone();
            let row_check_interval = row_check_interval.clone();

            move |step_id: &str| -> bool {
                let selected = combo_type.selected();
                let is_exchange = selected == 1;
                let is_goa = selected == 2;

                match step_id {
                    "identity" => {
                        let name = row_fullname.text().trim().to_string();
                        if name.is_empty() {
                            row_fullname.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie Ihren vollständigen Namen ein."));
                            }
                            return false;
                        }
                        let email = row_email.text().trim().to_string();
                        if !crate::services::account_config_service::is_plausible_address(&email) {
                            row_email.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie eine gültige E-Mail-Adresse ein."));
                            }
                            return false;
                        }
                        true
                    }
                    "receiving" => {
                        if is_goa {
                            return true;
                        }
                        let host = row_imap_host.text().trim().to_string();
                        if host.is_empty() {
                            row_imap_host.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                let msg = if is_exchange {
                                    "Bitte geben Sie die Rechneradresse (Host-URL) an (z. B. https://mail.../EWS/Exchange.asmx)."
                                } else {
                                    "Bitte geben Sie den Posteingangsserver an."
                                };
                                dlg.add_toast(&adw::Toast::new(msg));
                            }
                            return false;
                        }
                        let user = row_username.text().trim().to_string();
                        if user.is_empty() {
                            row_username.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie Ihren Benutzernamen ein."));
                            }
                            return false;
                        }
                        let pwd = row_password.text().to_string();
                        if pwd.is_empty() {
                            row_password.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie Ihr Passwort ein."));
                            }
                            return false;
                        }
                        if is_exchange {
                            let email = row_email.text().trim().to_string();
                            if row_open_other_mailbox.is_active() {
                                let other_mailbox = row_other_mailbox_user.text().trim().to_string();
                                if other_mailbox.is_empty() {
                                    row_other_mailbox_user.set_text(&email);
                                }
                            } else if !user.is_empty() && !email.is_empty() && user.to_lowercase() != email.to_lowercase() {
                                row_open_other_mailbox.set_active(true);
                                row_other_mailbox_user.set_sensitive(true);
                                if row_other_mailbox_user.text().trim().is_empty() {
                                    row_other_mailbox_user.set_text(&email);
                                }
                            }
                        } else {
                            let port = row_imap_port.text().trim().parse::<u16>().unwrap_or(0);
                            if port == 0 {
                                row_imap_port.grab_focus();
                                if let Some(dlg) = dialog_weak.upgrade() {
                                    dlg.add_toast(&adw::Toast::new("Bitte geben Sie einen gültigen Server-Port an (1–65535)."));
                                }
                                return false;
                            }
                        }
                        true
                    }
                    "sending" => {
                        if is_exchange || is_goa {
                            return true;
                        }
                        let host = row_smtp_host.text().trim().to_string();
                        if host.is_empty() {
                            row_smtp_host.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie den Postausgangsserver (SMTP) an."));
                            }
                            return false;
                        }
                        let port = row_smtp_port.text().trim().parse::<u16>().unwrap_or(0);
                        if port == 0 {
                            row_smtp_port.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Bitte geben Sie einen gültigen SMTP-Port an (1–65535)."));
                            }
                            return false;
                        }
                        true
                    }
                    "defaults" => {
                        let interval = row_check_interval.text().trim().parse::<u32>().unwrap_or(0);
                        if interval == 0 {
                            row_check_interval.grab_focus();
                            if let Some(dlg) = dialog_weak.upgrade() {
                                dlg.add_toast(&adw::Toast::new("Das Abrufintervall muss mindestens 1 Minute betragen."));
                            }
                            return false;
                        }
                        true
                    }
                    _ => true,
                }
            }
        };

        let get_step_defs = {
            let combo_type = combo_type.clone();
            move || -> Vec<(&'static str, &'static str)> {
                let selected = combo_type.selected();
                if selected == 1 || selected == 2 {
                    // Exchange or GOA: skip sending
                    vec![
                        ("identity", "Identität"),
                        ("receiving", "Empfangen"),
                        ("defaults", "Standardordner"),
                        ("security", "Sicherheit"),
                    ]
                } else {
                    vec![
                        ("identity", "Identität"),
                        ("receiving", "Empfangen"),
                        ("sending", "Versenden"),
                        ("defaults", "Standardordner"),
                        ("security", "Sicherheit"),
                    ]
                }
            }
        };

        let update_protocol_visibility = {
            let row_nav_sending = row_nav_sending.clone();
            let row_nav_receiving = row_nav_receiving.clone();
            let lbl_badge = lbl_badge.clone();
            let combo_type = combo_type.clone();
            move || {
                let selected = combo_type.selected();
                let is_exchange = selected == 1;
                let is_goa = selected == 2;
                row_nav_sending.set_visible(!is_exchange && !is_goa);
                if let Some(child) = row_nav_receiving.child() {
                    if let Some(hbox) = child.downcast_ref::<gtk::Box>() {
                        if let Some(vbox) = hbox.last_child().and_then(|w| w.downcast::<gtk::Box>().ok()) {
                            if let Some(sub) = vbox.last_child().and_then(|w| w.downcast::<gtk::Label>().ok()) {
                                sub.set_label(if is_exchange {
                                    "Exchange Web Services (EWS)"
                                } else {
                                    "Server, EWS & Anmeldedaten"
                                });
                            }
                        }
                    }
                }
                lbl_badge.set_label(match selected {
                    1 => "Microsoft Exchange (EWS)",
                    2 => "GNOME-Onlinekonto",
                    3 => "POP3 / SMTP",
                    _ => "IMAP / SMTP",
                });
            }
        };
        update_protocol_visibility();

        let step_index = Rc::new(Cell::new(0usize));
        let max_reached_step = Rc::new(Cell::new(0usize));

        let update_wizard_ui = {
            let step_index = step_index.clone();
            let max_reached_step = max_reached_step.clone();
            let get_step_defs = get_step_defs.clone();
            let view_stack = view_stack.clone();
            let window_title = window_title.clone();
            let lbl_status = lbl_status.clone();
            let btn_back = btn_back.clone();
            let btn_next = btn_next.clone();
            let sidebar_list = sidebar_list.clone();
            let split_view = split_view.clone();

            Rc::new(move |new_step: usize| {
                let steps = get_step_defs();
                let clamped = new_step.min(steps.len().saturating_sub(1));
                step_index.set(clamped);
                if clamped > max_reached_step.get() {
                    max_reached_step.set(clamped);
                }

                let (page_id, page_title) = steps[clamped];
                view_stack.set_visible_child_name(page_id);
                btn_back.set_visible(clamped > 0);
                let is_last = clamped == steps.len() - 1;
                btn_next.set_label(if is_last { "Konto einrichten" } else { "Weiter" });

                window_title.set_subtitle(&format!("Schritt {} von {}: {}", clamped + 1, steps.len(), page_title));
                lbl_status.set_label(&format!("Schritt {} von {}", clamped + 1, steps.len()));

                // Select matching row in sidebar
                let mut i = 0;
                while let Some(row) = sidebar_list.row_at_index(i) {
                    let r_page = unsafe {
                        row.data::<String>("page-name")
                            .map(|s| s.as_ref().clone())
                            .unwrap_or_default()
                    };
                    if r_page == page_id {
                        sidebar_list.select_row(Some(&row));
                        break;
                    }
                    i += 1;
                }
                split_view.set_show_content(true);
            })
        };

        if is_editing {
            let do_save = do_save.clone();
            btn_finish.connect_clicked(move |_| {
                let _ = do_save();
            });

            sidebar_list.connect_row_selected(glib::clone!(
                #[weak]
                view_stack,
                #[weak]
                window_title,
                #[weak]
                split_view,
                move |_, row| {
                    if let Some(row) = row {
                        let page_name = unsafe {
                            row.data::<String>("page-name")
                                .map(|s| s.as_ref().clone())
                                .unwrap_or_default()
                        };
                        let page_title = unsafe {
                            row.data::<String>("page-title")
                                .map(|s| s.as_ref().clone())
                                .unwrap_or_default()
                        };
                        if !page_name.is_empty() {
                            view_stack.set_visible_child_name(&page_name);
                            window_title.set_subtitle(&page_title);
                            split_view.set_show_content(true);
                        }
                    }
                }
            ));
        } else {
            {
                let step_index = step_index.clone();
                let get_step_defs = get_step_defs.clone();
                let validate_step = validate_step.clone();
                let update_wizard_ui = update_wizard_ui.clone();
                let do_save = do_save.clone();

                btn_next.connect_clicked(move |_| {
                    let steps = get_step_defs();
                    let cur = step_index.get();
                    if cur < steps.len() {
                        let (cur_id, _) = steps[cur];
                        if !validate_step(cur_id) {
                            return;
                        }
                        if cur == steps.len() - 1 {
                            let _ = do_save();
                            return;
                        }
                        update_wizard_ui(cur + 1);
                    }
                });
            }

            {
                let step_index = step_index.clone();
                let update_wizard_ui = update_wizard_ui.clone();
                btn_back.connect_clicked(move |_| {
                    let cur = step_index.get();
                    if cur > 0 {
                        update_wizard_ui(cur - 1);
                    }
                });
            }

            {
                let step_index = step_index.clone();
                let max_reached = max_reached_step.clone();
                let get_step_defs = get_step_defs.clone();
                let validate_step = validate_step.clone();
                let update_wizard_ui = update_wizard_ui.clone();
                let sidebar_list_ref = sidebar_list.clone();

                sidebar_list.connect_row_selected(move |_, row| {
                    let Some(row) = row else { return; };
                    let page_name = unsafe {
                        row.data::<String>("page-name")
                            .map(|s| s.as_ref().clone())
                            .unwrap_or_default()
                    };
                    let steps = get_step_defs();
                    if let Some(target_idx) = steps.iter().position(|(id, _)| *id == page_name) {
                        let cur = step_index.get();
                        if target_idx <= cur || target_idx <= max_reached.get() {
                            update_wizard_ui(target_idx);
                        } else {
                            // User clicked ahead: validate current step first
                            let (cur_id, _) = steps[cur];
                            if validate_step(cur_id) {
                                update_wizard_ui(target_idx);
                            } else {
                                // Keep selection on current step
                                let cur_page = steps[cur].0;
                                let mut i = 0;
                                while let Some(r) = sidebar_list_ref.row_at_index(i) {
                                    let p = unsafe {
                                        r.data::<String>("page-name")
                                            .map(|s| s.as_ref().clone())
                                            .unwrap_or_default()
                                    };
                                    if p == cur_page {
                                        sidebar_list_ref.select_row(Some(&r));
                                        break;
                                    }
                                    i += 1;
                                }
                            }
                        }
                    }
                });
            }

            {
                let update_protocol_visibility = update_protocol_visibility.clone();
                let step_index = step_index.clone();
                let update_wizard_ui = update_wizard_ui.clone();
                combo_type.connect_selected_notify(move |_| {
                    update_protocol_visibility();
                    let cur = step_index.get();
                    update_wizard_ui(cur);
                });
            }

            // Start at step 0 (Identität) in setup mode
            update_wizard_ui(0);
        }

        toast_overlay.set_child(Some(&split_view));
        self.set_content(Some(&toast_overlay));
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

        // Verify two-column navigation sidebar layout
        assert!(source.contains("navigation-sidebar"));
        assert!(source.contains("sidebar"));
        assert!(source.contains("split_view"));
        assert!(source.contains("Optionale Informationen"));
        assert!(source.contains("URL abrufen"));
        assert!(source.contains("OAB-URL"));

        // Verify action buttons
        assert!(source.contains("Abbrechen"));
        assert!(source.contains("Speichern"));
        assert!(source.contains("Einrichten"));
    }

    #[test]
    fn account_setup_dialog_wizard_contract() {
        assert_eq!(
            WIZARD_STEPS_IMAP,
            ["identity", "receiving", "sending", "defaults", "security"]
        );
        assert_eq!(
            WIZARD_STEPS_EXCHANGE,
            ["identity", "receiving", "defaults", "security"]
        );

        let source = include_str!("account_setup_dialog.rs");
        assert!(source.contains("Willkommen beim Evolution-Einrichtungsassistenten"));
        assert!(source.contains("Schritt"));
        assert!(source.contains("Weiter"));
        assert!(source.contains("Konto einrichten"));
        assert!(source.contains("Zusammenfassung & Abschluss"));
        assert!(source.contains("Legitimierungsabfrage"));
        assert!(source.contains("Passwort im Schlüsselbund speichern"));
        assert!(source.contains("discover_and_validate_exchange_services"));
    }

    #[test]
    fn account_setup_dialog_edit_profile_roundtrip() {
        let mut profile = AccountProfile::default();
        profile.id = "test-acc-123".to_string();
        profile.display_name = "Max Mustermann".to_string();
        profile.email = "max@example.com".to_string();
        profile.imap_host = "imap.example.com".to_string();
        profile.oab_url = "https://mail.example.com/OAB/".to_string();
        profile.smtp_host = "smtp.example.com".to_string();
        profile.pgp_key_id = "0x9876FEDC".to_string();
        profile.pgp_always_sign = true;
        profile.open_mailbox_other_user = true;
        profile.foreign_mailbox = "team@example.com".to_string();

        let encoded = serde_json::to_string(&profile).expect("must serialize");
        let decoded = serde_json::from_str::<AccountProfile>(&encoded).expect("must deserialize");
        assert_eq!(decoded.id, "test-acc-123");
        assert_eq!(decoded.display_name, "Max Mustermann");
        assert_eq!(decoded.email, "max@example.com");
        assert_eq!(decoded.oab_url, "https://mail.example.com/OAB/");
        assert_eq!(decoded.pgp_key_id, "0x9876FEDC");
        assert!(decoded.pgp_always_sign);
        assert!(decoded.open_mailbox_other_user);
        assert_eq!(decoded.foreign_mailbox, "team@example.com");
    }

    #[test]
    fn account_setup_dialog_open_other_mailbox_contract() {
        let source = include_str!("account_setup_dialog.rs");
        assert!(source.contains("Freigegebenes Postfach / Gruppenpostfach öffnen"));
        assert!(source.contains("E-Mail-Adresse des Gruppenpostfachs"));
        assert!(source.contains("row_open_other_mailbox"));
        assert!(source.contains("row_other_mailbox_user"));
    }

    #[test]
    fn test_is_discovery_candidate_matrix() {
        // Valid candidate cases
        assert!(is_discovery_candidate("user@gmail.com").is_some());
        assert!(is_discovery_candidate("john.doe@sub.company.co.uk").is_some());
        assert!(is_discovery_candidate("test@posteo.de").is_some());
        assert!(is_discovery_candidate("alice@gmx.net").is_some());

        // Incomplete / intermediate typing states (must NOT trigger)
        assert!(is_discovery_candidate("").is_none());
        assert!(is_discovery_candidate("user").is_none());
        assert!(is_discovery_candidate("user@").is_none());
        assert!(is_discovery_candidate("user@g").is_none());
        assert!(is_discovery_candidate("user@gmail.").is_none());
        assert!(is_discovery_candidate("user@.com").is_none());
        assert!(is_discovery_candidate("user@domain.c").is_none()); // TLD too short

        // Malformed cases
        assert!(is_discovery_candidate("user@domain with spaces.com").is_none());
        assert!(is_discovery_candidate("user@@domain.com").is_none());
        assert!(is_discovery_candidate("user@domain..com").is_none());
    }

    #[test]
    fn test_account_setup_dialog_autofill_contract_strings() {
        let source = include_str!("account_setup_dialog.rs");
        assert!(source.contains("discovery_spinner"));
        assert!(source.contains("row_discovery_status"));
        assert!(source.contains("is_discovery_candidate"));
        assert!(source.contains("timeout_add_local_once"));
        assert!(source.contains("connect_entry_activated"));
        assert!(source.contains("btn_redetect"));
        assert!(source.contains("discovery_generation"));
        assert!(source.contains("discovery_cancellation"));
    }
}
