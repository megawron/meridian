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

use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, AccountProtocol,
};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct AccountSetupDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for AccountSetupDialog {
        const NAME: &'static str = "EvolutionAccountSetupDialog";
        type Type = super::AccountSetupDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for AccountSetupDialog {
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

    fn build_ui(&self) {
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

        let row_security = adw::ComboRow::builder()
            .title("Verschlüsselung")
            .model(&gtk::StringList::new(&[
                "SSL / TLS (Port 993 / 465)",
                "STARTTLS (Port 143 / 587)",
                "Keine Verschlüsselung",
            ]))
            .build();
        group_server.add(&row_security);

        content_box.append(&group_server);

        // ── 4. Credentials ──────────────────────────────────────────────────
        let group_cred = adw::PreferencesGroup::builder()
            .title("Authentifizierung")
            .build();

        let row_password = adw::PasswordEntryRow::builder().title("Passwort").build();
        row_password.set_text("");
        row_password.set_tooltip_text(Some(
            "Wird erst bei einer EDS/Camel-Anbindung an den Secret Service übergeben; nicht lokal gespeichert",
        ));
        group_cred.add(&row_password);

        content_box.append(&group_cred);

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
            row_imap_host,
            #[strong]
            row_smtp_host,
            #[strong]
            row_security,
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
                let profile = AccountProfile {
                    id: format!("account:{}", email.to_lowercase()),
                    display_name,
                    email,
                    protocol,
                    imap_host: row_imap_host.text().trim().to_string(),
                    smtp_host: row_smtp_host.text().trim().to_string(),
                    tls_mode: tls_mode.to_string(),
                    enabled: true,
                };

                match AccountConfigStore::default().upsert(profile) {
                    Ok(()) => {
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
