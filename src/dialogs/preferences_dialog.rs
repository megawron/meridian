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
use gtk::glib;

use crate::services::AppSettings;

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

        // Filter Rules Section
        let group_filters = adw::PreferencesGroup::builder()
            .title("Mail-Filter &amp; Sortierung")
            .description("Automatische Regeln zum Sortieren und Filtern eingehender Mails")
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
        page_accounts.add(&group_filters);

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

        page_appearance.add(&group_reading);

        // ── 3. Page: Sicherheit & PGP/S-MIME ───────────────────────────────
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
        self.add(&page_security);
    }
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}
