// SPDX-License-Identifier: GPL-3.0-or-later
//
// AccountSetupDialog — Two-column settings & setup editor for Mail accounts.
//
// Structured into 5 native Libadwaita pages matching Evolution upstream:
// - 1. Identität (Identity): Account type/protocol, User info, Signatures (with Optional expander)
// - 2. Empfangen (Receiving): IMAP/POP3/Exchange server, credentials, OAB & Fetch URL, passwords, sync options
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
        pub sidebar_list: RefCell<Option<gtk::ListBox>>,
        pub view_stack: RefCell<Option<adw::ViewStack>>,
        pub split_view: RefCell<Option<adw::NavigationSplitView>>,
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
        let split_view = adw::NavigationSplitView::builder()
            .min_sidebar_width(280.0)
            .max_sidebar_width(340.0)
            .sidebar_width_fraction(0.32)
            .build();
        self.imp().split_view.replace(Some(split_view.clone()));

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

        let btn_finish = gtk::Button::builder()
            .label(if is_editing { "Speichern" } else { "Einrichten" })
            .css_classes(vec!["suggested-action"])
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

        // ─────────────────────────────────────────────────────────────────────
        // ── SIDEBAR (Left Column): ToolbarView + Profile + Rich Navigation ────
        // ─────────────────────────────────────────────────────────────────────
        let sidebar_toolbar_view = adw::ToolbarView::new();
        let sidebar_header_bar = adw::HeaderBar::new();
        sidebar_header_bar.set_show_title(false);
        sidebar_header_bar.set_show_end_title_buttons(false);
        sidebar_header_bar.pack_start(&btn_cancel);
        sidebar_toolbar_view.add_top_bar(&sidebar_header_bar);

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
                "Postfach konfigurieren".to_string(),
                "IMAP / SMTP".to_string(),
                "Bereit zur Einrichtung".to_string(),
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

        // Account Profile Card at top of sidebar
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

        let sep = gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&sep);

        // Rich Navigation List
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

        let is_ews = edit_profile.as_ref().map(|p| p.protocol == AccountProtocol::Exchange).unwrap_or(false);
        let receiving_sub = if is_ews {
            "Exchange Web Services (EWS)"
        } else {
            "Server, EWS & Anmeldedaten"
        };
        let sending_sub = if is_ews {
            "Exchange-Übermittlung"
        } else {
            "Postausgang & Übermittlung"
        };

        let row_nav_identity = make_sidebar_row("avatar-default-symbolic", "Identität", "Name, Adresse & Signatur", "identity");
        let row_nav_receiving = make_sidebar_row("mail-unread-symbolic", "Empfangen", receiving_sub, "receiving");
        let row_nav_sending = make_sidebar_row("mail-send-symbolic", "Versenden", sending_sub, "sending");
        let row_nav_defaults = make_sidebar_row("folder-symbolic", "Standardordner", "Entwürfe, Archiv & Papierkorb", "defaults");
        let row_nav_security = make_sidebar_row("security-high-symbolic", "Sicherheit", "OpenPGP & S/MIME", "security");

        sidebar_list.append(&row_nav_identity);
        sidebar_list.append(&row_nav_receiving);
        sidebar_list.append(&row_nav_sending);
        sidebar_list.append(&row_nav_defaults);
        sidebar_list.append(&row_nav_security);
        sidebar_list.select_row(Some(&row_nav_identity));

        sidebar_box.append(&sidebar_list);

        // Spacer pushes status to the bottom
        let spacer = gtk::Box::builder().vexpand(true).build();
        sidebar_box.append(&spacer);

        // Status indicator at bottom of sidebar
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

        // ─────────────────────────────────────────────────────────────────────
        // ── CONTENT (Right Column): ToolbarView + ViewStack ──────────────────
        // ─────────────────────────────────────────────────────────────────────
        let content_toolbar_view = adw::ToolbarView::new();
        let content_header_bar = adw::HeaderBar::new();
        content_header_bar.pack_end(&btn_finish);

        let window_title = adw::WindowTitle::builder()
            .title(&window_title_str)
            .subtitle("Identität")
            .build();
        content_header_bar.set_title_widget(Some(&window_title));
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

        // Responsive fallback: For narrow dialogs, collapse sidebar
        let bp_narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 620sp").unwrap());
        bp_narrow.add_setter(&split_view, "collapsed", Some(&true.to_value()));
        self.add_breakpoint(bp_narrow);

        let nav_rows = vec![
            ("identity", row_nav_identity.clone()),
            ("receiving", row_nav_receiving.clone()),
            ("sending", row_nav_sending.clone()),
            ("defaults", row_nav_defaults.clone()),
            ("security", row_nav_security.clone()),
        ];

        let switch_to_page = {
            let view_stack = view_stack.clone();
            let sidebar_list = sidebar_list.clone();
            let nav_rows = nav_rows.clone();
            let split_view = split_view.clone();
            move |target: &str| {
                view_stack.set_visible_child_name(target);
                for (page_tag, row) in &nav_rows {
                    if *page_tag == target {
                        sidebar_list.select_row(Some(row));
                        break;
                    }
                }
                split_view.set_show_content(true);
            }
        };

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
                            .unwrap_or_else(|| "identity".to_string())
                    };
                    let page_title = unsafe {
                        row.data::<String>("page-title")
                            .map(|s| s.as_ref().clone())
                            .unwrap_or_else(|| "Identität".to_string())
                    };
                    view_stack.set_visible_child_name(&page_name);
                    window_title.set_subtitle(&page_title);
                    split_view.set_show_content(true);
                }
            }
        ));

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

        // Standard Signature inside Optional expander
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

            // Auto-expand if any optional fields are configured
            if !profile.reply_to.is_empty()
                || !profile.organization.is_empty()
                || !profile.aliases.is_empty()
                || profile.signature_id.is_some()
            {
                expander_optional.set_expanded(true);
            }
        }

        let lbl_name_clone = lbl_name.clone();
        let avatar_clone = avatar.clone();
        row_fullname.connect_changed(move |entry| {
            let t = entry.text();
            if !t.is_empty() {
                lbl_name_clone.set_label(&t);
                avatar_clone.set_text(Some(&t));
            }
        });

        let lbl_email_clone = lbl_email.clone();
        row_email.connect_changed(move |entry| {
            let t = entry.text();
            if !t.is_empty() {
                lbl_email_clone.set_label(&t);
            }
        });

        let lbl_badge_clone = lbl_badge.clone();
        combo_type.connect_selected_notify(move |combo| {
            let badge = match combo.selected() {
                1 => "Microsoft Exchange (EWS)",
                2 => "GNOME-Onlinekonto",
                3 => "POP3 / SMTP",
                _ => "IMAP / SMTP",
            };
            lbl_badge_clone.set_label(badge);
        });

        group_user.add(&expander_optional);
        page_identity.add(&group_user);

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

        // Exchange EWS Discovery action row
        let row_fetch_url = adw::ActionRow::builder()
            .title("Exchange-Webdienste ermitteln")
            .subtitle("EWS- und OAB-Endpunkte anhand des Hostnamens ermitteln und prüfen")
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

        let row_imap_port = adw::EntryRow::builder().title("Server-Port").build();
        group_server.add(&row_imap_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_imap_host.set_text(&profile.imap_host);
            row_oab_url.set_text(&profile.oab_url);
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

        // Action: Fetch URL for Exchange EWS
        {
            let row_imap_host_ref = row_imap_host.clone();
            let row_oab_url_ref = row_oab_url.clone();
            let row_fetch_url_ref = row_fetch_url.clone();
            let row_email_ref = row_email.clone();
            btn_fetch.connect_clicked(move |_| {
                let input_host = row_imap_host_ref.text().trim().to_string();
                let host_base = if input_host.is_empty() {
                    let email = row_email_ref.text().trim().to_string();
                    email.split('@').nth(1).unwrap_or("localhost").to_string()
                } else {
                    input_host
                        .strip_prefix("https://")
                        .or_else(|| input_host.strip_prefix("http://"))
                        .unwrap_or(&input_host)
                        .split('/')
                        .next()
                        .unwrap_or("localhost")
                        .to_string()
                };

                let ews_url = format!("https://{}/EWS/Exchange.asmx", host_base);
                let current_oab = row_oab_url_ref.text().trim().to_string();
                let clean_oab = crate::services::account_config_service::sanitize_oab_url(&current_oab);

                row_imap_host_ref.set_text(&ews_url);
                row_oab_url_ref.set_text(&clean_oab);
                row_fetch_url_ref.set_subtitle("EWS-Endpunkt ermittelt. OAB-URL ist optional (wird bei leerem Feld direkt über EWS abgefragt). Verbindung wird geprüft …");

                let (tx, rx) = async_channel::bounded::<String>(1);
                let check_url = ews_url.clone();
                std::thread::spawn(move || {
                    let client = reqwest::blocking::Client::builder()
                        .danger_accept_invalid_certs(true)
                        .timeout(std::time::Duration::from_secs(4))
                        .build();

                    let status_msg = match client {
                        Ok(c) => match c.get(&check_url).send() {
                            Ok(resp) => {
                                let code = resp.status().as_u16();
                                if code == 200 || code == 401 || code == 403 {
                                    format!("Server erreichbar (HTTP {}): EWS-Endpunkt aktiv (OAB optional)", code)
                                } else {
                                    format!("Server antwortete mit Status {}", code)
                                }
                            }
                            Err(err) => {
                                format!("URLs formatiert (Serverabfrage: {})", err)
                            }
                        },
                        Err(e) => format!("URLs formatiert ({})", e),
                    };

                    let _ = tx.send_blocking(status_msg);
                });

                let row_fetch_clone = row_fetch_url_ref.clone();
                glib::spawn_future_local(async move {
                    if let Ok(msg) = rx.recv().await {
                        row_fetch_clone.set_subtitle(&msg);
                    }
                });
            });
        }

        // 2b. Credentials
        let group_cred = adw::PreferencesGroup::builder()
            .title("Authentifizierung und Zugangsdaten")
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
                _ => row_auth_combo.set_selected(0), // Default NTLM
            }
        }
        group_cred.add(&row_auth_combo);

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

        page_receiving.add(&group_cred);

        // 2c. Check / Sync Options
        let group_sync = adw::PreferencesGroup::builder()
            .title("Abruf- und Bereinigungsoptionen")
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
            let row_fetch_url = row_fetch_url.clone();
            let row_oab_url = row_oab_url.clone();
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
                    "Exchange-Host / EWS-URL"
                } else if is_pop3 {
                    "Posteingangsserver (POP3)"
                } else if is_goa {
                    "Posteingangsserver (über Onlinekonto verwaltet)"
                } else {
                    "Posteingangsserver (IMAP)"
                });

                row_fetch_url.set_visible(is_exchange);
                row_oab_url.set_visible(is_exchange);
                row_imap_port.set_visible(!is_exchange && has_manual_servers);
                row_imap_port.set_sensitive(has_manual_servers);

                row_smtp_exchange_info.set_visible(is_exchange);
                row_smtp_host.set_visible(has_manual_smtp);
                row_smtp_port.set_visible(has_manual_smtp);
                row_smtp_auth_info.set_visible(has_manual_smtp);

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
                row_auth_combo.set_sensitive(!is_goa);
                row_password.set_sensitive(!is_goa);

                if is_exchange {
                    row_username.set_tooltip_text(Some(
                        "z. B. DOMÄNE\\Benutzername oder E-Mail-Adresse",
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
        combo_type.connect_selected_notify(move |combo| {
            update_protocol_fields(combo.selected());
        });

        // Set initial sidebar selection
        sidebar_list.select_row(Some(&row_nav_identity));

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
            #[strong]
            switch_to_page,
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
            row_oab_url,
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
            row_auth_combo,
            #[strong]
            row_password,
            #[strong]
            row_remember_password,
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
                    switch_to_page("identity");
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

                let auth_method = match row_auth_combo.selected() {
                    1 => "Basic".to_string(),
                    2 => "GSSAPI".to_string(),
                    3 => "OAuth2".to_string(),
                    _ => "NTLM".to_string(),
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
                };

                if let Err(error) = profile.validate() {
                    let err_lower = error.to_lowercase();
                    if err_lower.contains("imap")
                        || err_lower.contains("pop")
                        || err_lower.contains("empfang")
                        || err_lower.contains("port")
                        || err_lower.contains("exchange")
                    {
                        switch_to_page("receiving");
                    } else if err_lower.contains("smtp") || err_lower.contains("versand") {
                        switch_to_page("sending");
                    } else {
                        switch_to_page("identity");
                    }
                    if let Some(parent) = dialog.transient_for() {
                        if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                            window.add_toast(&adw::Toast::new(&error));
                        }
                    }
                    return;
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

                        // Sync to Evolution Data Server (.source files & D-Bus reload) and Keyring
                        let pwd_opt = if remember_password && !password.is_empty() {
                            Some(password.as_str())
                        } else {
                            None
                        };
                        match crate::services::account_config_service::sync_profile_to_eds(&profile, pwd_opt) {
                            Ok(eds_uid) => {
                                tracing::info!(%eds_uid, "Account synced to EDS and Keyring successfully");
                            }
                            Err(err) => {
                                tracing::warn!(%err, "EDS synchronization reported notice");
                            }
                        }

                        // Notify parent window to reload mail accounts in the sidebar
                        if let Some(parent) = dialog.transient_for() {
                            if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>()
                            {
                                window.reload_mail_accounts();
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

        self.set_content(Some(&split_view));
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

        let encoded = serde_json::to_string(&profile).expect("must serialize");
        let decoded = serde_json::from_str::<AccountProfile>(&encoded).expect("must deserialize");
        assert_eq!(decoded.id, "test-acc-123");
        assert_eq!(decoded.display_name, "Max Mustermann");
        assert_eq!(decoded.email, "max@example.com");
        assert_eq!(decoded.oab_url, "https://mail.example.com/OAB/");
        assert_eq!(decoded.pgp_key_id, "0x9876FEDC");
        assert!(decoded.pgp_always_sign);
    }
}
