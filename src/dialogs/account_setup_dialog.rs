// SPDX-License-Identifier: GPL-3.0-or-later
//
// AccountSetupDialog — Minimalist single-column account setup & configuration dialog
// following GNOME HIG and Progressive Disclosure principles.
//
// - New accounts: Focused single-page view (Name, Email, Password) with automatic
//   Autodiscover and a collapsed "Server & Verbindung" expander for manual tweaks.
// - Existing accounts: Single-column preferences page where advanced settings
//   (Server ports/TLS/EWS, Special Folders, OpenPGP & S/MIME, Optional Identity)
//   are progressively disclosed via AdwExpanderRows.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};

use crate::services::account_config_service::{
    AccountConfigStore, AccountProfile, AccountProtocol,
};
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct AccountSetupDialog {
        pub edit_profile_json: RefCell<Option<String>>,
        pub expander_optional: RefCell<Option<adw::ExpanderRow>>,
        pub nav_view: RefCell<Option<adw::NavigationView>>,
        pub row_nav_server: RefCell<Option<adw::ActionRow>>,
        pub row_nav_sync: RefCell<Option<adw::ActionRow>>,
        pub label_nav_server: RefCell<Option<gtk::Label>>,
        pub label_nav_sync: RefCell<Option<gtk::Label>>,
        pub toast_overlay: RefCell<Option<adw::ToastOverlay>>,
        pub discovery_generation: std::cell::Cell<u64>,
        pub discovery_cancellation: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub debounce_source: RefCell<Option<glib::SourceId>>,
        pub last_discovered_email: RefCell<String>,
        pub modified: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AccountSetupDialog {
        const NAME: &'static str = "MeridianAccountSetupDialog";
        type Type = super::AccountSetupDialog;
        type ParentType = adw::Dialog;
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
    impl AdwDialogImpl for AccountSetupDialog {
        fn closed(&self) {
            if let Some(cancellation) = self.discovery_cancellation.borrow().as_ref() {
                cancellation.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(source) = self.debounce_source.borrow_mut().take() {
                source.remove();
            }
        }
    }
}

glib::wrapper! {
    pub struct AccountSetupDialog(ObjectSubclass<imp::AccountSetupDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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

const ACCOUNT_SYNC_INTERVAL_MINUTES: &[u32] = &[0, 1, 5, 10, 15, 30, 60];

pub(crate) fn account_sync_interval_to_index(
    follow_global_sync: bool,
    auto_check_enabled: bool,
    minutes: u32,
) -> u32 {
    if follow_global_sync {
        return 0;
    }
    if !auto_check_enabled || minutes == 0 {
        return 1;
    }
    ACCOUNT_SYNC_INTERVAL_MINUTES[1..]
        .iter()
        .enumerate()
        .min_by_key(|&(_, &val)| (val as i64 - minutes as i64).abs())
        .map(|(idx, _)| (idx + 2) as u32)
        .unwrap_or(4)
}

pub(crate) fn index_to_account_sync_interval(index: u32, fallback_minutes: u32) -> (bool, bool, u32) {
    match index {
        0 => (true, true, fallback_minutes.max(1)),
        1 => (false, false, fallback_minutes.max(1)),
        idx => {
            let mins = ACCOUNT_SYNC_INTERVAL_MINUTES
                .get((idx - 1) as usize)
                .copied()
                .unwrap_or(10);
            (false, true, mins)
        }
    }
}

fn collect_known_account_folders(profile: Option<&AccountProfile>) -> Vec<String> {
    let mut folders = Vec::new();
    let Some(profile) = profile else {
        return folders;
    };

    let mut seen = std::collections::HashSet::new();

    let live_summaries = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|w| w.downcast::<crate::window::MeridianWindow>().ok())
        .and_then(|win| win.mail_page())
        .map(|mp| mp.cached_folder_summaries())
        .unwrap_or_default();

    let eds_uid = crate::services::account_config_service::eds_source_uid_for_email(&profile.email);
    let prof_canon = profile.id.strip_prefix("eds:").unwrap_or(&profile.id);
    let eds_canon = eds_uid.strip_prefix("eds:").unwrap_or(&eds_uid);

    let matching_live: Vec<_> = live_summaries
        .iter()
        .filter(|f| {
            let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
            f.is_selectable
                && !f.folder_name.starts_with('.')
                && !f.folder_name.contains("/.")
                && f.folder_name != "[Gmail]"
                && !f.folder_name.eq_ignore_ascii_case("[Google Mail]")
                && (f_canon == prof_canon
                    || (!eds_canon.is_empty() && f_canon == eds_canon)
                    || f.source_display_name.eq_ignore_ascii_case(&profile.display_name)
                    || f.source_display_name.eq_ignore_ascii_case(&profile.email))
        })
        .collect();

    for f in matching_live {
        if seen.insert(f.folder_name.clone()) {
            folders.push(f.folder_name.clone());
        }
    }

    if folders.is_empty() {
        let hint = if !profile.id.is_empty() {
            Some(profile.id.as_str())
        } else if !profile.email.is_empty() {
            Some(profile.email.as_str())
        } else {
            None
        };
        if let Some(cache) = crate::services::evolution_summary_cache::find_evolution_account_cache(hint) {
            if let Ok(sqlite_folders) = crate::services::evolution_summary_cache::load_folders_from_sqlite(
                &cache.folders_db,
                &cache.source_uid,
                &cache.display_name,
            ) {
                for f in sqlite_folders {
                    if f.is_selectable
                        && !f.folder_name.starts_with('.')
                        && !f.folder_name.contains("/.")
                        && f.folder_name != "[Gmail]"
                        && !f.folder_name.eq_ignore_ascii_case("[Google Mail]")
                    {
                        if seen.insert(f.folder_name.clone()) {
                            folders.push(f.folder_name.clone());
                        }
                    }
                }
            }
        }
    }

    folders
}

fn create_folder_combo_row(title: &str, known_folders: &[String], stored_value: &str) -> adw::ComboRow {
    let mut options = vec!["Automatic".to_string()];
    for f in known_folders {
        if !options.contains(f) {
            options.push(f.clone());
        }
    }
    let trimmed = stored_value.trim();
    if !trimmed.is_empty() && !options.iter().any(|opt| opt == trimmed) {
        options.push(trimmed.to_string());
    }
    let selected_idx = if trimmed.is_empty() {
        0
    } else {
        options.iter().position(|opt| opt == trimmed).unwrap_or(0) as u32
    };

    let option_refs: Vec<&str> = options.iter().map(String::as_str).collect();
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&option_refs))
        .selected(selected_idx)
        .build()
}

fn folder_combo_value(combo: &adw::ComboRow) -> String {
    let sel = combo.selected();
    if sel == 0 {
        String::new()
    } else {
        combo
            .model()
            .and_then(|m| m.downcast::<gtk::StringList>().ok())
            .and_then(|sl| sl.string(sel))
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

fn configure_natural_scroll_height(widget: &gtk::Widget, max_height: i32) {
    if let Some(scrolled) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        scrolled.set_propagate_natural_height(true);
        scrolled.set_max_content_height(max_height);
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        configure_natural_scroll_height(&c, max_height);
        child = c.next_sibling();
    }
}

impl AccountSetupDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Add Account")
            .property("content-width", 480)
            .property("content-height", 640)
            .property("follows-content-size", false)
            .build()
    }

    pub fn new_for_edit(profile: &AccountProfile) -> Self {
        let is_goa = profile.protocol == AccountProtocol::Goa || profile.id.starts_with("eds:");
        let (width, height) = if is_goa { (460, 620) } else { (480, 640) };
        let encoded = serde_json::to_string(profile).expect("account profile should serialize");
        glib::Object::builder()
            .property("title", "Account Settings")
            .property("content-width", width)
            .property("content-height", height)
            .property("follows-content-size", false)
            .property("edit-profile-json", encoded)
            .build()
    }

    pub fn was_modified(&self) -> bool {
        self.imp().modified.get()
    }

    pub fn add_toast(&self, toast: &adw::Toast) {
        if let Some(overlay) = self.imp().toast_overlay.borrow().as_ref() {
            overlay.add_toast(toast.clone());
        } else if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(toast);
        }
    }

    pub fn select_page(&self, target: &str) {
        let Some(nav) = self.imp().nav_view.borrow().clone() else {
            return;
        };
        match target {
            "receiving" | "sending" | "server" => {
                if nav.find_page("server").is_some() {
                    nav.push_by_tag("server");
                }
            }
            "defaults" | "sync" => {
                if nav.find_page("sync").is_some() {
                    nav.push_by_tag("sync");
                }
            }
            _ => {}
        }
    }

    fn build_ui(&self) {
        let mut edit_profile = self
            .imp()
            .edit_profile_json
            .borrow()
            .as_deref()
            .and_then(|raw| serde_json::from_str::<AccountProfile>(raw).ok());

        if let Some(ref mut profile) = edit_profile {
            if profile.protocol == AccountProtocol::Exchange && profile.imap_host.trim().is_empty() {
                let resolved = profile
                    .parent_account_id
                    .as_deref()
                    .and_then(|parent_id| {
                        AccountConfigStore::default()
                            .load()
                            .into_iter()
                            .find(|p| p.id == parent_id && !p.imap_host.trim().is_empty())
                            .map(|p| (p.imap_host, p.oab_url, p.auth_method, p.username))
                            .or_else(|| {
                                crate::services::account_config_service::read_eds_ews_connection_info(
                                    parent_id,
                                )
                            })
                    })
                    .or_else(|| {
                        crate::services::account_config_service::read_eds_ews_connection_info(
                            &profile.id,
                        )
                    })
                    .or_else(|| {
                        crate::services::account_config_service::read_eds_ews_connection_info(
                            &profile.email,
                        )
                    });

                if let Some((host_url, oab_url, auth_method, username)) = resolved {
                    profile.imap_host = host_url;
                    if profile.oab_url.trim().is_empty() && !oab_url.is_empty() {
                        profile.oab_url = oab_url;
                    }
                    if profile.auth_method.trim().is_empty() && !auth_method.is_empty() {
                        profile.auth_method = auth_method;
                    }
                    if profile.username.trim().is_empty() && !username.is_empty() {
                        profile.username = username;
                    }
                    let _ = AccountConfigStore::default().upsert(profile.clone());
                }
            }
        }

        let is_editing = edit_profile.is_some();

        // ─────────────────────────────────────────────────────────────────────
        // ── 1. FORM FIELDS (Minimalist Single-Column + Progressive Disclosure)
        // ─────────────────────────────────────────────────────────────────────

        let goa_available = crate::services::eds_service::is_gnome_online_accounts_available();
        let protocol_labels: Vec<&str> = if goa_available || is_editing {
            vec![
                "IMAP and SMTP (Standard)",
                "Microsoft Exchange (EWS)",
                "GNOME Online Account (GOA)",
                "POP3 and SMTP",
            ]
        } else {
            vec![
                "IMAP and SMTP (Standard)",
                "Microsoft Exchange (EWS)",
                "POP3 and SMTP",
            ]
        };

        let combo_type = adw::ComboRow::builder()
            .title("Account Type")
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

        let map_combo_idx_to_protocol = move |idx: u32| -> AccountProtocol {
            if goa_available || is_editing {
                match idx {
                    1 => AccountProtocol::Exchange,
                    2 => AccountProtocol::Goa,
                    3 => AccountProtocol::Pop3,
                    _ => AccountProtocol::ImapSmtp,
                }
            } else {
                match idx {
                    1 => AccountProtocol::Exchange,
                    2 => AccountProtocol::Pop3,
                    _ => AccountProtocol::ImapSmtp,
                }
            }
        };

        let update_combo_subtitle = {
            let combo_type_ref = combo_type.clone();
            move |idx: u32| {
                let text = match map_combo_idx_to_protocol(idx) {
                    AccountProtocol::Exchange => "Microsoft Exchange (EWS)",
                    AccountProtocol::Goa => "GNOME Online Account (GOA)",
                    AccountProtocol::Pop3 => "POP3 and SMTP",
                    AccountProtocol::ImapSmtp => "IMAP and SMTP (Standard)",
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

        let is_editing_goa = edit_profile
            .as_ref()
            .is_some_and(|p| p.protocol == AccountProtocol::Goa || p.id.starts_with("eds:"));
        if is_editing_goa {
            self.set_content_width(460);
            self.set_content_height(620);
        } else {
            self.set_content_width(480);
            self.set_content_height(640);
        }
        let min_dialog_width = if is_editing_goa { 460 } else { 480 };
        let is_editing_shared = edit_profile
            .as_ref()
            .is_some_and(|p| p.is_shared_mailbox);
        let goa_uid_str = edit_profile
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let resolved_goa_id = goa_uid_str
            .strip_prefix("eds:")
            .and_then(|u| crate::services::eds_service::resolve_goa_account_info(u))
            .map(|(id, _)| id)
            .unwrap_or_default();

        let group_goa_services = adw::PreferencesGroup::builder()
            .title("GNOME Online Account")
            .visible(is_editing_goa)
            .build();
        let btn_goa_header = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text("Open in GNOME Settings")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .visible(goa_available)
            .build();
        let goa_id_for_header = resolved_goa_id.clone();
        btn_goa_header.connect_clicked(move |_| {
            crate::services::eds_service::open_gnome_online_accounts(Some(&goa_id_for_header));
        });
        group_goa_services.set_header_suffix(Some(&btn_goa_header));

        let (initial_goa_mail, initial_goa_cal, initial_goa_contacts) = if is_editing_goa {
            crate::services::eds_service::get_goa_account_services_state(&goa_uid_str)
        } else {
            (true, true, true)
        };

        let syncing_from_goa = std::rc::Rc::new(std::cell::Cell::new(false));

        let row_goa_mail = adw::SwitchRow::builder()
            .title("Mail")
            .active(initial_goa_mail)
            .build();
        let uid_for_mail = goa_uid_str.clone();
        let dlg_for_mail = self.downgrade();
        let syncing_for_mail = syncing_from_goa.clone();
        row_goa_mail.connect_active_notify(move |sw| {
            if syncing_for_mail.get() {
                return;
            }
            crate::services::eds_service::set_goa_account_service_enabled(
                &uid_for_mail,
                crate::services::eds_service::EdsSourceKind::Mail,
                sw.is_active(),
            );
            if let Some(dlg) = dlg_for_mail.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&dlg) {
                    win.reload_accounts();
                }
            }
        });
        group_goa_services.add(&row_goa_mail);

        let row_goa_cal = adw::SwitchRow::builder()
            .title("Calendar")
            .active(initial_goa_cal)
            .build();
        let uid_for_cal = goa_uid_str.clone();
        let dlg_for_cal = self.downgrade();
        let syncing_for_cal = syncing_from_goa.clone();
        row_goa_cal.connect_active_notify(move |sw| {
            if syncing_for_cal.get() {
                return;
            }
            crate::services::eds_service::set_goa_account_service_enabled(
                &uid_for_cal,
                crate::services::eds_service::EdsSourceKind::Calendar,
                sw.is_active(),
            );
            if let Some(dlg) = dlg_for_cal.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&dlg) {
                    win.reload_accounts();
                }
            }
        });
        group_goa_services.add(&row_goa_cal);

        let row_goa_contacts = adw::SwitchRow::builder()
            .title("Contacts")
            .active(initial_goa_contacts)
            .build();
        let uid_for_con = goa_uid_str.clone();
        let dlg_for_con = self.downgrade();
        let syncing_for_con = syncing_from_goa.clone();
        row_goa_contacts.connect_active_notify(move |sw| {
            if syncing_for_con.get() {
                return;
            }
            crate::services::eds_service::set_goa_account_service_enabled(
                &uid_for_con,
                crate::services::eds_service::EdsSourceKind::AddressBook,
                sw.is_active(),
            );
            if let Some(dlg) = dlg_for_con.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&dlg) {
                    win.reload_accounts();
                }
            }
        });
        group_goa_services.add(&row_goa_contacts);

        // Live bi-directional sync: listen to org.gnome.OnlineAccounts D-Bus PropertiesChanged
        if is_editing_goa && !resolved_goa_id.is_empty() {
            if let Ok(bus) =
                gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
            {
                let obj_path = format!("/org/gnome/OnlineAccounts/Accounts/{resolved_goa_id}");
                let uid_for_sig = goa_uid_str.clone();
                let mail_sw_weak = row_goa_mail.downgrade();
                let cal_sw_weak = row_goa_cal.downgrade();
                let con_sw_weak = row_goa_contacts.downgrade();
                let dlg_for_sig = self.downgrade();
                let syncing_for_sig = syncing_from_goa.clone();

                let sub_id = bus.signal_subscribe(
                    Some("org.gnome.OnlineAccounts"),
                    Some("org.freedesktop.DBus.Properties"),
                    Some("PropertiesChanged"),
                    Some(&obj_path),
                    None,
                    gtk::gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, _, _| {
                        let (mail_on, cal_on, con_on) =
                            crate::services::eds_service::get_goa_account_services_state(
                                &uid_for_sig,
                            );
                        syncing_for_sig.set(true);
                        if let Some(sw) = mail_sw_weak.upgrade() {
                            if sw.is_active() != mail_on {
                                sw.set_active(mail_on);
                            }
                        }
                        if let Some(sw) = cal_sw_weak.upgrade() {
                            if sw.is_active() != cal_on {
                                sw.set_active(cal_on);
                            }
                        }
                        if let Some(sw) = con_sw_weak.upgrade() {
                            if sw.is_active() != con_on {
                                sw.set_active(con_on);
                            }
                        }
                        syncing_for_sig.set(false);
                        if let Some(dlg) = dlg_for_sig.upgrade() {
                            if let Some(win) = crate::window::find_meridian_window(&dlg) {
                                win.reload_accounts();
                            }
                        }
                    },
                );
                let bus_for_close = bus.clone();
                let sub_id_cell = std::cell::Cell::new(Some(sub_id));
                self.connect_closed(move |_| {
                    if let Some(id) = sub_id_cell.take() {
                        bus_for_close.signal_unsubscribe(id);
                    }
                });
            }
        }

        let group_user = adw::PreferencesGroup::builder()
            .title(if is_editing_goa { "" } else { "Account & Identity" })
            .build();
        combo_type.set_visible(!is_editing_goa && !is_editing_shared);
        group_user.add(&combo_type);

        let row_goa_info = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("GNOME Online Account"))
            .activatable(true)
            .visible(false)
            .build();
        let btn_open_goa = gtk::Button::builder()
            .label(&crate::i18n::gettext("Open GNOME Settings"))
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .build();
        btn_open_goa.connect_clicked(|_| {
            crate::services::eds_service::open_gnome_online_accounts(None);
        });
        row_goa_info.connect_activated(|_| {
            crate::services::eds_service::open_gnome_online_accounts(None);
        });
        row_goa_info.add_suffix(&btn_open_goa);
        group_user.add(&row_goa_info);

        let row_fullname = adw::EntryRow::builder().title("Full Name").build();
        group_user.add(&row_fullname);

        let row_email = adw::EntryRow::builder().title("Email Address").build();

        // ── Autodiscover Status Indicator & Spinner ───────────────────────────
        let discovery_spinner = gtk::Spinner::builder()
            .spinning(false)
            .visible(false)
            .valign(gtk::Align::Center)
            .tooltip_text("Discovering server settings…")
            .build();
        if !is_editing {
            row_email.add_suffix(&discovery_spinner);
        }
        group_user.add(&row_email);

        let signatures = crate::services::signature_store_service::SignatureStore::from_environment().load();
        let signatures_state = std::rc::Rc::new(std::cell::RefCell::new(signatures));
        let mut sig_labels = vec![
            "Use Default Signature".to_string(),
            "No Signature".to_string(),
        ];
        for sig in signatures_state.borrow().iter() {
            sig_labels.push(sig.name.clone());
        }
        sig_labels.push("New Signature…".to_string());
        let sig_refs: Vec<&str> = sig_labels.iter().map(String::as_str).collect();
        let row_signature = adw::ComboRow::builder()
            .title("Signature")
            .model(&gtk::StringList::new(&sig_refs))
            .visible(true)
            .build();
        let selected_sig_idx = match edit_profile
            .as_ref()
            .and_then(|p| p.signature_id.as_deref())
        {
            None | Some("") | Some("default") => 0,
            Some("none") => 1,
            Some(sig_id) => signatures_state
                .borrow()
                .iter()
                .position(|s| s.id == *sig_id)
                .map(|p| (p + 2) as u32)
                .unwrap_or(0),
        };
        row_signature.set_selected(selected_sig_idx);
        let prev_selected_sig_idx = std::rc::Rc::new(std::cell::Cell::new(selected_sig_idx));
        let is_updating_sig_combo = std::rc::Rc::new(std::cell::Cell::new(false));

        {
            let is_updating = is_updating_sig_combo.clone();
            let prev_idx = prev_selected_sig_idx.clone();
            let sigs_rc = signatures_state.clone();
            let combo_weak = row_signature.downgrade();
            let dialog_weak = self.downgrade();

            row_signature.connect_selected_notify(move |combo| {
                if is_updating.get() {
                    return;
                }
                let sel = combo.selected();
                let sigs_count = sigs_rc.borrow().len();
                let new_sig_idx = (sigs_count + 2) as u32;

                if sel == new_sig_idx {
                    let Some(dialog) = dialog_weak.upgrade() else { return };
                    let prev = prev_idx.get();

                    let saved = std::rc::Rc::new(std::cell::Cell::new(false));
                    let saved_for_cb = saved.clone();
                    let combo_for_cb = combo_weak.clone();
                    let sigs_for_cb = sigs_rc.clone();
                    let prev_for_cb = prev_idx.clone();
                    let is_updating_for_cb = is_updating.clone();

                    let editor = crate::dialogs::SignatureEditorDialog::new_for_create(Some(move |created_sig: crate::services::signature_store_service::SignatureProfile| {
                        saved_for_cb.set(true);
                        let Some(c) = combo_for_cb.upgrade() else { return };

                        let loaded = crate::services::signature_store_service::SignatureStore::from_environment().load();
                        let new_pos = loaded
                            .iter()
                            .position(|s| s.id == created_sig.id)
                            .map(|p| (p + 2) as u32)
                            .unwrap_or(0);
                        *sigs_for_cb.borrow_mut() = loaded.clone();

                        let mut labels = vec![
                            "Use Default Signature".to_string(),
                            "No Signature".to_string(),
                        ];
                        for sig in &loaded {
                            labels.push(sig.name.clone());
                        }
                        labels.push("New Signature…".to_string());
                        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();

                        is_updating_for_cb.set(true);
                        c.set_model(Some(&gtk::StringList::new(&refs)));
                        c.set_selected(new_pos);
                        prev_for_cb.set(new_pos);
                        is_updating_for_cb.set(false);
                    }));

                    let saved_for_closed = saved.clone();
                    let combo_for_closed = combo_weak.clone();
                    let is_updating_for_closed = is_updating.clone();
                    editor.connect_closed(move |_| {
                        if !saved_for_closed.get() {
                            if let Some(c) = combo_for_closed.upgrade() {
                                is_updating_for_closed.set(true);
                                c.set_selected(prev);
                                is_updating_for_closed.set(false);
                            }
                        }
                    });

                    editor.present(Some(&dialog));
                } else {
                    prev_idx.set(sel);
                }
            });
        }

        group_user.add(&row_signature);

        let row_discovery_status = adw::ActionRow::builder()
            .title("Server Settings")
            .subtitle("Enter your email address to detect server settings automatically")
            .visible(false)
            .build();

        let btn_redetect = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .valign(gtk::Align::Center)
            .has_frame(false)
            .tooltip_text("Redetect server settings")
            .visible(false)
            .build();
        row_discovery_status.add_suffix(&btn_redetect);
        group_user.add(&row_discovery_status);

        // Smart OAuth / GOA recommendation banner for Google & Microsoft accounts
        let row_oauth_goa_hint = adw::ActionRow::builder()
            .title("Sign in via GNOME Online Accounts")
            .activatable(true)
            .visible(false)
            .build();
        let icon_oauth_goa = gtk::Image::builder()
            .icon_name("goa-account-google")
            .pixel_size(24)
            .valign(gtk::Align::Center)
            .build();
        row_oauth_goa_hint.add_prefix(&icon_oauth_goa);
        let btn_oauth_goa = gtk::Button::builder()
            .label("Sign In with GNOME…")
            .valign(gtk::Align::Center)
            .css_classes(vec!["suggested-action"])
            .build();
        let dlg_for_oauth_hint = self.downgrade();
        btn_oauth_goa.connect_clicked(move |_| {
            crate::services::eds_service::open_gnome_online_accounts(None);
            if let Some(dlg) = dlg_for_oauth_hint.upgrade() {
                dlg.close();
            }
        });
        let dlg_for_oauth_row = self.downgrade();
        row_oauth_goa_hint.connect_activated(move |_| {
            crate::services::eds_service::open_gnome_online_accounts(None);
            if let Some(dlg) = dlg_for_oauth_row.upgrade() {
                dlg.close();
            }
        });
        row_oauth_goa_hint.add_suffix(&btn_oauth_goa);
        group_user.add(&row_oauth_goa_hint);

        let row_password = adw::PasswordEntryRow::builder()
            .title("Password")
            .build();
        row_password.set_tooltip_text(Some(
            "Stored securely in the system keyring (Secret Service)",
        ));
        group_user.add(&row_password);

        let row_account_enabled = adw::SwitchRow::builder()
            .title("Account Enabled")
            .active(edit_profile.as_ref().map(|p| p.enabled).unwrap_or(true))
            .visible(is_editing && !is_editing_goa)
            .build();

        let is_currently_default = edit_profile.as_ref().is_some_and(|p| {
            AppSettings::try_new()
                .map(|s| s.default_account_id() == p.id)
                .unwrap_or(false)
        });
        let row_account_default = adw::SwitchRow::builder()
            .title("Use as Default Account")
            .active(is_currently_default)
            .visible(false)
            .build();
        group_user.add(&row_account_default);

        // Aliases & Reply-To (minimalist ExpanderRow without legacy clutter)
        let expander_optional = adw::ExpanderRow::builder()
            .title("Aliases & Reply-To")
            .use_markup(false)
            .expanded(false)
            .build();
        self.imp()
            .expander_optional
            .replace(Some(expander_optional.clone()));

        let row_aliases = adw::EntryRow::builder()
            .title("Aliases (comma-separated)")
            .build();
        expander_optional.add_row(&row_aliases);

        let row_reply_to = adw::EntryRow::builder()
            .title("Reply-To Address")
            .build();
        expander_optional.add_row(&row_reply_to);

        let row_organization = adw::EntryRow::builder()
            .title("Organization")
            .visible(false)
            .build();
        expander_optional.add_row(&row_organization);

        let row_default_cc = adw::EntryRow::builder()
            .title("Always CC")
            .visible(false)
            .build();
        expander_optional.add_row(&row_default_cc);

        let row_default_bcc = adw::EntryRow::builder()
            .title("Always BCC")
            .visible(false)
            .build();
        expander_optional.add_row(&row_default_bcc);

        let row_prefer_html = adw::SwitchRow::builder()
            .title("Prefer HTML formatting when composing")
            .active(edit_profile.as_ref().map(|p| p.prefer_html).unwrap_or(true))
            .visible(false)
            .build();
        expander_optional.add_row(&row_prefer_html);

        if let Some(profile) = edit_profile.as_ref() {
            row_fullname.set_text(&profile.display_name);
            row_email.set_text(&profile.email);
            row_reply_to.set_text(&profile.reply_to);
            row_organization.set_text(&profile.organization);
            row_aliases.set_text(&profile.aliases.join(", "));
            row_default_cc.set_text(&profile.default_cc);
            row_default_bcc.set_text(&profile.default_bcc);
        }
        group_user.add(&expander_optional);

        // ── 1b. Server & Connection Subpage (Libadwaita Navigation Page) ─────
        let server_page_content = adw::PreferencesPage::new();
        server_page_content.set_width_request(min_dialog_width);
        let group_server = adw::PreferencesGroup::builder()
            .title("Connection Details")
            .build();
        server_page_content.add(&group_server);

        let row_imap_host = adw::EntryRow::builder()
            .title("Incoming Server (IMAP)")
            .build();
        let btn_fetch = gtk::Button::builder()
            .label("Fetch URL")
            .valign(gtk::Align::Center)
            .build();
        row_imap_host.add_suffix(&btn_fetch);
        group_server.add(&row_imap_host);

        // OAB URL Row (Offline Address Book)
        let row_oab_url = adw::EntryRow::builder()
            .title("Offline Address Book")
            .build();
        group_server.add(&row_oab_url);

        let row_username = adw::EntryRow::builder().title("Username").build();
        if let Some(profile) = edit_profile.as_ref() {
            let user_text = if profile.protocol == AccountProtocol::Exchange {
                crate::services::account_config_service::format_exchange_domain_user(
                    &profile.username,
                    &profile.email,
                )
            } else {
                profile.username.clone()
            };
            row_username.set_text(&user_text);
        }
        group_server.add(&row_username);

        let row_auth_combo = adw::ComboRow::builder()
            .title("Authentication Method")
            .model(&gtk::StringList::new(&[
                "NTLM",
                "Password",
                "Kerberos / GSSAPI",
                "OAuth2",
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
        group_server.add(&row_auth_combo);

        let row_security = adw::ComboRow::builder()
            .title("Encryption")
            .model(&gtk::StringList::new(&[
                "SSL / TLS",
                "STARTTLS",
                "None",
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

        let row_open_other_mailbox = adw::SwitchRow::builder()
            .title("Open Shared / Delegated Mailbox")
            .active(false)
            .build();
        group_server.add(&row_open_other_mailbox);

        let row_other_mailbox_user = adw::EntryRow::builder()
            .title("Shared Mailbox Email Address")
            .build();
        group_server.add(&row_other_mailbox_user);

        let row_other_user_ref = row_other_mailbox_user.clone();
        row_open_other_mailbox.connect_active_notify(move |switch| {
            row_other_user_ref.set_sensitive(switch.is_active());
        });
        row_other_mailbox_user.set_sensitive(false);

        let row_imap_port = adw::EntryRow::builder().title("Server Port").build();
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

        let row_remember_password = adw::SwitchRow::builder()
            .title("Save password in keyring")
            .active(true)
            .build();
        group_server.add(&row_remember_password);

        // Action: Authenticated Fetch URL for Exchange EWS with Authentication Request popup
        {
            let row_imap_host_ref = row_imap_host.clone();
            let row_oab_url_ref = row_oab_url.clone();
            let row_open_other_mailbox_ref = row_open_other_mailbox.clone();
            let row_other_mailbox_user_ref = row_other_mailbox_user.clone();
            let row_email_ref = row_email.clone();
            let row_username_ref = row_username.clone();
            let row_password_ref = row_password.clone();
            let row_remember_password_ref = row_remember_password.clone();
            let row_auth_combo_ref = row_auth_combo.clone();
            let btn_fetch_ref = btn_fetch.clone();
            let dialog_weak = self.downgrade();

            btn_fetch.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };

                let email = row_email_ref.text().trim().to_string();
                let initial_user = crate::services::account_config_service::format_exchange_domain_user(
                    &row_username_ref.text(),
                    &email,
                );
                let initial_pwd = row_password_ref.text().to_string();

                let alert = adw::AlertDialog::new(
                    Some("Authentication Required"),
                    Some("Enter your credentials:"),
                );

                let content_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .width_request(380)
                    .build();

                let group_prompt = adw::PreferencesGroup::new();

                let entry_user = adw::EntryRow::builder()
                    .title("Domain\\Username")
                    .text(&initial_user)
                    .build();
                group_prompt.add(&entry_user);

                let entry_pwd = adw::PasswordEntryRow::builder()
                    .title("Password")
                    .text(&initial_pwd)
                    .build();
                group_prompt.add(&entry_pwd);

                let switch_save = adw::SwitchRow::builder()
                    .title("Save password in keyring")
                    .active(row_remember_password_ref.is_active())
                    .build();
                group_prompt.add(&switch_save);

                content_box.append(&group_prompt);
                alert.set_extra_child(Some(&content_box));

                alert.add_response("cancel", &crate::i18n::gettext("Cancel"));
                alert.add_response("confirm", &crate::i18n::gettext("Sign In"));
                alert.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
                alert.set_default_response(Some("confirm"));
                alert.set_close_response("cancel");

                let row_email_ref = row_email_ref.clone();
                let row_username_ref = row_username_ref.clone();
                let row_password_ref = row_password_ref.clone();
                let row_remember_password_ref = row_remember_password_ref.clone();
                let row_imap_host_ref = row_imap_host_ref.clone();
                let row_oab_url_ref = row_oab_url_ref.clone();
                let row_open_other_mailbox_ref = row_open_other_mailbox_ref.clone();
                let row_other_mailbox_user_ref = row_other_mailbox_user_ref.clone();
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
                        if let Some(dlg) = dialog_weak_inner.upgrade() {
                            dlg.add_toast(&adw::Toast::new("Please enter username and password"));
                        }
                        return;
                    }

                    btn_fetch_ref.set_sensitive(false);
                    let input_host = row_imap_host_ref.text().trim().to_string();
                    let email_for_thread = row_email_ref.text().trim().to_string();

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

                    let row_imap_host_clone = row_imap_host_ref.clone();
                    let row_oab_url_clone = row_oab_url_ref.clone();
                    let row_auth_combo_clone = row_auth_combo_ref.clone();
                    let btn_fetch_clone = btn_fetch_ref.clone();
                    let dialog_weak_async = dialog_weak_inner.clone();

                    glib::spawn_future_local(async move {
                        btn_fetch_clone.set_sensitive(true);
                        if let Ok(res) = rx.recv().await {
                            match res {
                                crate::services::account_config_service::ExchangeDiscoveryStatus::Success { ews_url, oab_url, message: _ } => {
                                    row_imap_host_clone.set_text(&ews_url);
                                    row_oab_url_clone.set_text(&oab_url);
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new("Exchange endpoints discovered successfully"));
                                    }

                                    let host_for_auth = ews_url.clone();
                                    let row_auth_combo_async = row_auth_combo_clone.clone();
                                    let (tx_auth, rx_auth) = async_channel::bounded::<Result<Vec<String>, String>>(1);
                                    std::thread::spawn(move || {
                                        let res = crate::services::account_config_service::query_exchange_auth_methods(&host_for_auth);
                                        let _ = tx_auth.send_blocking(res);
                                    });
                                    glib::spawn_future_local(async move {
                                        if let Ok(Ok(methods)) = rx_auth.recv().await {
                                            if methods.iter().any(|m| m.contains("NTLM")) {
                                                row_auth_combo_async.set_selected(0);
                                            } else if methods.iter().any(|m| m.contains("Kerberos")) {
                                                row_auth_combo_async.set_selected(2);
                                            } else if methods.iter().any(|m| m.contains("OAuth2")) {
                                                row_auth_combo_async.set_selected(3);
                                            } else if methods.iter().any(|m| m.contains("Basic")) {
                                                row_auth_combo_async.set_selected(1);
                                            }
                                        }
                                    });
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::MissingCredentials => {
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new("Please enter username and password"));
                                    }
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::AuthFailed(_msg) => {
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new("Authentication failed: check password or try DOMAIN\\username"));
                                    }
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::ServerNotFound(msg) => {
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new(&msg));
                                    }
                                    row_imap_host_clone.grab_focus();
                                }
                                crate::services::account_config_service::ExchangeDiscoveryStatus::Error(msg) => {
                                    if let Some(dlg) = dialog_weak_async.upgrade() {
                                        dlg.add_toast(&adw::Toast::new(&msg));
                                    }
                                }
                            }
                        }
                    });
                });

                alert.present(Some(&dialog));
                entry_pwd.grab_focus();
            });
        }

        // Sending (SMTP) inside group_server
        let row_smtp_host = adw::EntryRow::builder()
            .title("Outgoing Server (SMTP)")
            .build();
        group_server.add(&row_smtp_host);

        let row_smtp_port = adw::EntryRow::builder().title("SMTP Port").build();
        group_server.add(&row_smtp_port);

        if let Some(profile) = edit_profile.as_ref() {
            row_smtp_host.set_text(&profile.smtp_host);
            row_smtp_port.set_text(&profile.smtp_port.to_string());
        }

        // Navigation row on the main page pointing to Server Settings subpage
        let row_nav_server = adw::ActionRow::builder()
            .title("Server Settings")
            .activatable(true)
            .build();

        let initial_server_summary = if is_editing {
            if let Some(profile) = edit_profile.as_ref() {
                if profile.protocol == AccountProtocol::Exchange {
                    if !profile.imap_host.is_empty() {
                        profile.imap_host.clone()
                    } else {
                        "Exchange".to_string()
                    }
                } else if !profile.imap_host.is_empty() {
                    format!("{}:{}", profile.imap_host, profile.imap_port)
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let label_nav_server = gtk::Label::builder()
            .label(&initial_server_summary)
            .css_classes(vec!["dim-label"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .valign(gtk::Align::Center)
            .build();
        let icon_server_chevron = gtk::Image::from_icon_name("go-next-symbolic");
        icon_server_chevron.add_css_class("dim-label");

        row_nav_server.add_suffix(&label_nav_server);
        row_nav_server.add_suffix(&icon_server_chevron);
        self.imp().row_nav_server.replace(Some(row_nav_server.clone()));
        self.imp().label_nav_server.replace(Some(label_nav_server.clone()));

        // ── 1c. Synchronization Subpage (Libadwaita Navigation Page) ──────────
        let sync_page_content = adw::PreferencesPage::new();
        sync_page_content.set_width_request(min_dialog_width);
        let group_sync = adw::PreferencesGroup::builder()
            .title("Message Checking")
            .build();
        sync_page_content.add(&group_sync);

        let sync_interval_labels = [
            crate::i18n::gettext("Use Default"),
            crate::i18n::gettext("Manually"),
            crate::i18n::gettext("Every minute"),
            crate::i18n::gettext("Every 5 minutes"),
            crate::i18n::gettext("Every 10 minutes"),
            crate::i18n::gettext("Every 15 minutes"),
            crate::i18n::gettext("Every 30 minutes"),
            crate::i18n::gettext("Hourly"),
        ];
        let sync_interval_refs: Vec<&str> =
            sync_interval_labels.iter().map(String::as_str).collect();
        let initial_follow_global = edit_profile
            .as_ref()
            .map(|p| p.follow_global_sync)
            .unwrap_or(true);
        let initial_auto_check = edit_profile
            .as_ref()
            .map(|p| p.auto_check_enabled)
            .unwrap_or(true);
        let initial_interval_mins = edit_profile
            .as_ref()
            .map(|p| p.check_interval_minutes)
            .unwrap_or(10);
        let initial_sync_idx = account_sync_interval_to_index(
            initial_follow_global,
            initial_auto_check,
            initial_interval_mins,
        );
        let row_check_interval_combo = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Check for new messages"))
            .model(&gtk::StringList::new(&sync_interval_refs))
            .selected(initial_sync_idx)
            .build();
        group_sync.add(&row_check_interval_combo);

        let row_check_all_folders = adw::SwitchRow::builder()
            .title("Check all folders for new messages")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.check_all_folders)
                    .unwrap_or(false),
            )
            .build();
        group_sync.add(&row_check_all_folders);

        let group_trash = adw::PreferencesGroup::builder()
            .title("Trash & Maintenance")
            .build();
        sync_page_content.add(&group_trash);

        let row_expunge_on_exit = adw::SwitchRow::builder()
            .title("Empty trash on exit (Expunge)")
            .active(
                edit_profile
                    .as_ref()
                    .map(|p| p.expunge_on_exit)
                    .unwrap_or(false),
            )
            .build();
        group_trash.add(&row_expunge_on_exit);

        let initial_sync_summary = if initial_sync_idx == 0 {
            "Default".to_string()
        } else {
            sync_interval_labels
                .get(initial_sync_idx as usize)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Default".to_string())
        };

        // Navigation row on the main page pointing to Synchronization subpage
        let row_nav_sync = adw::ActionRow::builder()
            .title("Synchronization")
            .activatable(true)
            .visible(is_editing && !is_editing_goa)
            .build();

        let label_nav_sync = gtk::Label::builder()
            .label(&initial_sync_summary)
            .css_classes(vec!["dim-label"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .valign(gtk::Align::Center)
            .build();
        let icon_sync_chevron = gtk::Image::from_icon_name("go-next-symbolic");
        icon_sync_chevron.add_css_class("dim-label");

        row_nav_sync.add_suffix(&label_nav_sync);
        row_nav_sync.add_suffix(&icon_sync_chevron);
        self.imp().row_nav_sync.replace(Some(row_nav_sync.clone()));
        self.imp().label_nav_sync.replace(Some(label_nav_sync.clone()));

        {
            let label_nav_sync = label_nav_sync.clone();
            let sync_interval_labels = sync_interval_labels.clone();
            row_check_interval_combo.connect_selected_notify(move |combo| {
                let idx = combo.selected() as usize;
                if idx == 0 {
                    label_nav_sync.set_text("Default");
                } else if let Some(label) = sync_interval_labels.get(idx) {
                    label_nav_sync.set_text(label);
                }
            });
        }

        // Group for subpage navigation rows
        let group_nav = adw::PreferencesGroup::new();
        group_nav.add(&row_nav_server);
        group_nav.add(&row_nav_sync);
        group_nav.set_visible(!is_editing_goa);

        let row_inbox_folder = adw::EntryRow::builder().title("Inbox").visible(false).build();
        let known_folders = collect_known_account_folders(edit_profile.as_ref());
        let stored_drafts = edit_profile.as_ref().map(|p| p.drafts_folder.as_str()).unwrap_or("");
        let stored_sent = edit_profile.as_ref().map(|p| p.sent_folder.as_str()).unwrap_or("");
        let stored_archive = edit_profile.as_ref().map(|p| p.archive_folder.as_str()).unwrap_or("");
        let stored_trash = edit_profile.as_ref().map(|p| p.trash_folder.as_str()).unwrap_or("");
        let stored_junk = edit_profile.as_ref().map(|p| p.junk_folder.as_str()).unwrap_or("");

        let row_drafts_folder = create_folder_combo_row("Drafts", &known_folders, stored_drafts);
        let row_sent_folder = create_folder_combo_row("Sent", &known_folders, stored_sent);
        let row_archive_folder = create_folder_combo_row("Archive", &known_folders, stored_archive);
        let row_trash_folder = create_folder_combo_row("Trash", &known_folders, stored_trash);
        let row_junk_folder = create_folder_combo_row("Junk", &known_folders, stored_junk);

        group_trash.add(&row_inbox_folder);

        let group_special_folders = adw::PreferencesGroup::new();
        let expander_folders = adw::ExpanderRow::builder()
            .title("Folders")
            .build();
        expander_folders.add_row(&row_drafts_folder);
        expander_folders.add_row(&row_sent_folder);
        expander_folders.add_row(&row_archive_folder);
        expander_folders.add_row(&row_trash_folder);
        expander_folders.add_row(&row_junk_folder);
        group_special_folders.add(&expander_folders);
        sync_page_content.add(&group_special_folders);

        if let Some(profile) = edit_profile.as_ref() {
            row_inbox_folder.set_text(&profile.inbox_folder);
        }

        // ─────────────────────────────────────────────────────────────────────
        // ── 2. PROTOCOL FIELDS DYNAMICS ───────────────────────────────────────
        // ─────────────────────────────────────────────────────────────────────
        let update_protocol_fields = {
            let row_goa_info = row_goa_info.clone();
            let group_goa_services = group_goa_services.clone();
            let group_server = group_server.clone();
            let row_nav_server = row_nav_server.clone();
            let row_imap_host = row_imap_host.clone();
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
            let row_remember_password = row_remember_password.clone();
            let row_password = row_password.clone();
            let row_email = row_email.clone();

            move |selected: u32| {
                let proto = map_combo_idx_to_protocol(selected);
                let is_exchange = proto == AccountProtocol::Exchange;
                let is_goa = proto == AccountProtocol::Goa;
                let is_pop3 = proto == AccountProtocol::Pop3;
                let has_manual_servers = !is_goa;
                let has_manual_smtp = !is_goa && !is_exchange;

                row_goa_info.set_visible(is_goa && !is_editing);
                group_goa_services.set_visible(is_goa && is_editing);
                group_server.set_visible(!is_goa);
                row_nav_server.set_visible(!is_goa);
                row_password.set_visible(!is_goa && !is_editing_shared);
                row_remember_password.set_visible(!is_goa && !is_editing_shared);
                row_email.set_editable(!is_goa || !is_editing);

                row_imap_host.set_sensitive(has_manual_servers);
                row_imap_host.set_title(if is_exchange {
                    "Host URL"
                } else if is_pop3 {
                    "Incoming Server (POP3)"
                } else if is_goa {
                    "Incoming Server (managed by Online Account)"
                } else {
                    "Incoming Server (IMAP)"
                });

                if is_exchange {
                    row_imap_host.set_tooltip_text(Some("Exchange server address (e.g. mail.company.com or https://mail.company.com/EWS/Exchange.asmx)"));
                } else {
                    row_imap_host.set_tooltip_text(None);
                }

                row_oab_url.set_visible(is_exchange);
                row_open_other_mailbox.set_visible(is_exchange && !is_editing_shared);
                row_other_mailbox_user.set_visible(is_exchange && !is_editing_shared && row_open_other_mailbox.is_active());
                row_imap_port.set_visible(!is_exchange && has_manual_servers);
                row_imap_port.set_sensitive(has_manual_servers);

                row_smtp_host.set_visible(has_manual_smtp);
                row_smtp_port.set_visible(has_manual_smtp);

                row_security.set_sensitive(has_manual_servers);

                row_inbox_folder.set_sensitive(true);
                row_drafts_folder.set_sensitive(true);
                row_sent_folder.set_sensitive(true);
                row_archive_folder.set_sensitive(true);
                row_trash_folder.set_sensitive(true);
                row_junk_folder.set_sensitive(true);

                row_username.set_sensitive(!is_goa);
                row_auth_combo.set_sensitive(!is_goa);
                row_password.set_sensitive(!is_goa);

                if is_exchange {
                    row_username.set_title("Domain\\Username");
                    row_username.set_tooltip_text(Some(
                        "DOMAIN\\username or email address",
                    ));
                } else if is_goa {
                    row_username.set_title("Username");
                    row_username.set_tooltip_text(Some(
                        "The online account manages username and credentials",
                    ));
                } else {
                    row_username.set_title("Username");
                    row_username.set_tooltip_text(None);
                }
            }
        };

        update_protocol_fields(combo_type.selected());
        {
            let update_clone = update_protocol_fields.clone();
            let group_nav_clone = group_nav.clone();
            let is_goa_initial = is_editing_goa;
            group_nav.set_visible(!is_goa_initial);
            combo_type.connect_selected_notify(move |combo| {
                let sel = combo.selected();
                update_clone(sel);
                let proto = map_combo_idx_to_protocol(sel);
                group_nav_clone.set_visible(proto != AccountProtocol::Goa);
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
                row_discovery_status.set_title("Searching for server settings…");
                row_discovery_status.set_subtitle(&format!("Looking up settings for “{domain}”…"));
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
                                row_discovery_status_clone.set_title("Server settings found");
                                row_discovery_status_clone.set_subtitle(&format!(
                                    "✓ {} ({}:{} · {}:{})",
                                    config.display_name,
                                    config.imap_host(),
                                    config.imap_port(),
                                    config.smtp_host(),
                                    config.smtp_port()
                                ));
                                if let Some(label) = dialog.imp().label_nav_server.borrow().as_ref() {
                                    label.set_text(&format!(
                                        "{}:{} · {}:{}",
                                        config.imap_host(),
                                        config.imap_port(),
                                        config.smtp_host(),
                                        config.smtp_port()
                                    ));
                                }
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
                                row_discovery_status_clone.set_title("Default settings suggested");
                                row_discovery_status_clone.set_subtitle(&format!(
                                    "Could not automatically detect server settings for “{domain}”. Please verify the server details below."
                                ));
                                if let Some(label) = dialog.imp().label_nav_server.borrow().as_ref() {
                                    label.set_text(&format!("imap.{domain}:993 · smtp.{domain}:587"));
                                }

                                dialog.add_toast(&adw::Toast::new(
                                    &format!("Could not detect settings for {domain} — suggested defaults filled in"),
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
        let row_oauth_goa_hint_clone = row_oauth_goa_hint.clone();
        let icon_oauth_goa_clone = icon_oauth_goa.clone();
        row_email.connect_changed(move |entry| {
            let t = entry.text();

            let Some(dialog) = dialog_weak.upgrade() else { return; };
            let raw = t.trim().to_string();

            if !is_editing && goa_available {
                let lower = raw.to_ascii_lowercase();
                if lower.ends_with("@gmail.com") || lower.ends_with("@googlemail.com") {
                    icon_oauth_goa_clone.set_icon_name(Some("goa-account-google"));
                    row_oauth_goa_hint_clone.set_title("Connect Google Account via GNOME");
                    row_oauth_goa_hint_clone.set_visible(true);
                } else if lower.ends_with("@outlook.com")
                    || lower.ends_with("@hotmail.com")
                    || lower.ends_with("@live.com")
                    || lower.ends_with("@office365.com")
                    || lower.ends_with("@msn.com")
                {
                    icon_oauth_goa_clone.set_icon_name(Some("goa-account-ms365"));
                    row_oauth_goa_hint_clone.set_title("Connect Microsoft Account via GNOME");
                    row_oauth_goa_hint_clone.set_visible(true);
                } else {
                    row_oauth_goa_hint_clone.set_visible(false);
                }
            }

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
        let edit_is_shared = edit_profile
            .as_ref()
            .map(|profile| profile.is_shared_mailbox)
            .unwrap_or(false);
        let edit_parent_account_id = edit_profile
            .as_ref()
            .and_then(|profile| profile.parent_account_id.clone());
        let edit_signature_id = edit_profile
            .as_ref()
            .and_then(|profile| profile.signature_id.clone());
        let edit_pgp_key_id = edit_profile
            .as_ref()
            .map(|p| p.pgp_key_id.clone())
            .unwrap_or_default();
        let edit_pgp_always_sign = edit_profile
            .as_ref()
            .map(|p| p.pgp_always_sign)
            .unwrap_or(false);
        let edit_pgp_always_encrypt = edit_profile
            .as_ref()
            .map(|p| p.pgp_always_encrypt)
            .unwrap_or(false);
        let edit_smime_sign_cert = edit_profile
            .as_ref()
            .map(|p| p.smime_sign_cert.clone())
            .unwrap_or_default();
        let edit_smime_encrypt_cert = edit_profile
            .as_ref()
            .map(|p| p.smime_encrypt_cert.clone())
            .unwrap_or_default();

        let do_save = {
            let dialog = self.downgrade();
            let combo_type = combo_type.clone();
            let row_fullname = row_fullname.clone();
            let row_email = row_email.clone();
            let row_account_enabled = row_account_enabled.clone();
            let row_account_default = row_account_default.clone();
            let row_reply_to = row_reply_to.clone();
            let row_organization = row_organization.clone();
            let row_aliases = row_aliases.clone();
            let row_signature = row_signature.clone();
            let signatures_state = signatures_state.clone();
            let row_imap_host = row_imap_host.clone();
            let row_oab_url = row_oab_url.clone();
            let row_open_other_mailbox = row_open_other_mailbox.clone();
            let row_other_mailbox_user = row_other_mailbox_user.clone();
            let row_smtp_host = row_smtp_host.clone();
            let row_imap_port = row_imap_port.clone();
            let row_smtp_port = row_smtp_port.clone();
            let row_check_interval_combo = row_check_interval_combo.clone();
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
            let row_prefer_html = row_prefer_html.clone();
            let row_default_cc = row_default_cc.clone();
            let row_default_bcc = row_default_bcc.clone();

            move || -> bool {
                let Some(dialog) = dialog.upgrade() else {
                    return false;
                };

                let mut email = row_email.text().trim().to_string();
                let mut display_name = row_fullname.text().trim().to_string();
                let mut username = row_username.text().trim().to_string();

                if email.is_empty() && username.contains('@') {
                    email = username.clone();
                    row_email.set_text(&email);
                }

                if email.is_empty() || !crate::services::account_config_service::is_plausible_address(&email) {
                    dialog.add_toast(&adw::Toast::new("Please enter a valid email address"));
                    row_email.grab_focus();
                    return false;
                }

                let protocol = map_combo_idx_to_protocol(combo_type.selected());

                if display_name.is_empty() {
                    display_name = if let Some((user, _)) = email.split_once('@') {
                        user.to_string()
                    } else {
                        email.clone()
                    };
                    row_fullname.set_text(&display_name);
                }

                if username.is_empty() {
                    username = email.clone();
                    row_username.set_text(&username);
                }

                let mut imap_host = row_imap_host.text().trim().to_string();
                let mut smtp_host = row_smtp_host.text().trim().to_string();

                // If user clicked save before autodiscover finished, try synchronous lookup or domain heuristic
                if imap_host.is_empty() && protocol == AccountProtocol::ImapSmtp {
                    if let Some((_, domain)) = email.split_once('@') {
                        let domain = domain.trim();
                        if !domain.is_empty() {
                            imap_host = format!("imap.{domain}");
                            row_imap_host.set_text(&imap_host);
                        }
                    }
                }
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
                    0 => Some("default".to_string()),
                    1 => Some("none".to_string()),
                    idx => {
                        let sigs = signatures_state.borrow();
                        if (idx as usize) >= 2 && (idx as usize) < sigs.len() + 2 {
                            sigs.get((idx - 2) as usize)
                                .map(|s| s.id.clone())
                                .or_else(|| edit_signature_id.clone())
                        } else {
                            edit_signature_id.clone().or_else(|| Some("default".to_string()))
                        }
                    }
                };

                let (follow_global_sync, auto_check_enabled, check_interval_minutes) = index_to_account_sync_interval(
                    row_check_interval_combo.selected(),
                    initial_interval_mins,
                );

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
                    drafts_folder: folder_combo_value(&row_drafts_folder),
                    sent_folder: folder_combo_value(&row_sent_folder),
                    archive_folder: folder_combo_value(&row_archive_folder),
                    trash_folder: folder_combo_value(&row_trash_folder),
                    junk_folder: folder_combo_value(&row_junk_folder),
                    enabled: if is_editing { row_account_enabled.is_active() } else { true },
                    signature_id,
                    follow_global_sync,
                    check_interval_minutes,
                    auto_check_enabled,
                    check_all_folders: row_check_all_folders.is_active(),
                    expunge_on_exit: row_expunge_on_exit.is_active(),
                    pgp_key_id: edit_pgp_key_id.clone(),
                    pgp_always_sign: edit_pgp_always_sign,
                    pgp_always_encrypt: edit_pgp_always_encrypt,
                    smime_sign_cert: edit_smime_sign_cert.clone(),
                    smime_encrypt_cert: edit_smime_encrypt_cert.clone(),
                    default_cc: row_default_cc.text().trim().to_string(),
                    default_bcc: row_default_bcc.text().trim().to_string(),
                    prefer_html: row_prefer_html.is_active(),
                    open_mailbox_other_user,
                    foreign_mailbox,
                    is_shared_mailbox: open_mailbox_other_user || edit_is_shared,
                    parent_account_id: edit_parent_account_id.clone(),
                };

                if let Err(error) = profile.validate() {
                    dialog.select_page("server");
                    dialog.add_toast(&adw::Toast::new(&error));
                    return false;
                }

                let profile_id = profile.id.clone();
                let password = row_password.text().to_string();
                let remember_password = row_remember_password.is_active();

                match AccountConfigStore::default().upsert(profile.clone()) {
                    Ok(()) => {
                        dialog.imp().modified.set(true);
                        if let Some(settings) = AppSettings::try_new() {
                            if (is_editing && row_account_default.is_active())
                                || settings.default_account_id().trim().is_empty()
                            {
                                settings.set_default_account_id(&profile_id);
                            }
                        }

                        let pwd_opt = if remember_password && !password.is_empty() {
                            Some(password.clone())
                        } else {
                            None
                        };
                        let profile_for_eds = profile.clone();
                        std::thread::spawn(move || {
                            let _ = crate::services::account_config_service::sync_profile_to_eds(
                                &profile_for_eds,
                                pwd_opt.as_deref(),
                            );
                        });

                        let dialog_for_idle = dialog.clone();
                        dialog.close();
                        glib::idle_add_local_once(move || {
                            if let Some(window) = crate::window::find_meridian_window(&dialog_for_idle) {
                                window.reload_accounts();
                                window.add_toast(&adw::Toast::new(if is_editing {
                                    "Account settings saved"
                                } else {
                                    "Account added successfully"
                                }));
                            }
                        });
                        true
                    }
                    Err(error) => {
                        tracing::error!(%error, "Could not persist account profile");
                        dialog.add_toast(&adw::Toast::new(&format!(
                            "Could not save account profile: {error}"
                        )));
                        false
                    }
                }
            }
        };

        // ─────────────────────────────────────────────────────────────────────
        // ── 4. MINIMALIST SINGLE-COLUMN LAYOUT (Setup & Edit) ────────────────
        // ─────────────────────────────────────────────────────────────────────
        let toast_overlay = adw::ToastOverlay::new();
        self.imp().toast_overlay.replace(Some(toast_overlay.clone()));

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        header_bar.set_show_start_title_buttons(false);
        header_bar.set_show_end_title_buttons(false);

        let btn_cancel = gtk::Button::builder()
            .label(&crate::i18n::gettext("Cancel"))
            .build();
        btn_cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                dialog.close();
            }
        ));
        header_bar.pack_start(&btn_cancel);

        let (window_title_str, window_subtitle_str) = if let Some(ref profile) = edit_profile {
            let title = if !profile.display_name.is_empty() {
                profile.display_name.clone()
            } else {
                "Account Settings".to_string()
            };
            (title, profile.email.clone())
        } else {
            ("Add Account".to_string(), String::new())
        };

        let window_title = adw::WindowTitle::builder()
            .title(&window_title_str)
            .subtitle(&window_subtitle_str)
            .build();
        header_bar.set_title_widget(Some(&window_title));

        let btn_save = gtk::Button::builder()
            .label(if is_editing { "Save" } else { "Set Up Account" })
            .css_classes(vec!["suggested-action"])
            .build();
        {
            let do_save = do_save.clone();
            btn_save.connect_clicked(move |_| {
                let _ = do_save();
            });
        }
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let main_page_content = adw::PreferencesPage::new();
        main_page_content.set_width_request(min_dialog_width);
        main_page_content.add(&group_goa_services);
        main_page_content.add(&group_user);
        main_page_content.add(&group_nav);

        // Account lifecycle section at the bottom when editing a local profile
        if let Some(ref profile) = edit_profile {
            if !profile.id.starts_with("eds:") {
                let group_danger = adw::PreferencesGroup::new();
                group_danger.add(&row_account_enabled);
                let row_delete = adw::ActionRow::builder()
                    .title("Remove Account…")
                    .activatable(true)
                    .build();
                row_delete.add_css_class("error");

                let profile_id = profile.id.clone();
                let profile_name = if !profile.display_name.is_empty() {
                    profile.display_name.clone()
                } else {
                    profile.email.clone()
                };
                let dialog_weak = self.downgrade();
                row_delete.connect_activated(move |_| {
                    let Some(dialog) = dialog_weak.upgrade() else {
                        return;
                    };
                    let dlg_for_delete = dialog.clone();
                    let profile_id = profile_id.clone();
                    crate::dialogs::run_destructive_action(
                        &dialog,
                        &format!("Remove account “{profile_name}”?"),
                        "The account profile will be removed from Meridian. Messages on the mail server will not be deleted.",
                        "Remove",
                        move || {
                            if let Ok(true) = AccountConfigStore::default().delete(&profile_id) {
                                dlg_for_delete.imp().modified.set(true);
                                let dlg_close = dlg_for_delete.clone();
                                dlg_for_delete.close();
                                glib::idle_add_local_once(move || {
                                    if let Some(window) = crate::window::find_meridian_window(&dlg_close) {
                                        window.reload_accounts();
                                        window.add_toast(&adw::Toast::new("Account removed"));
                                    }
                                });
                            }
                        },
                    );
                });
                group_danger.add(&row_delete);
                main_page_content.add(&group_danger);
            }
        }

        let clamp = adw::Clamp::builder()
            .maximum_size(520)
            .tightening_threshold(440)
            .child(&main_page_content)
            .build();
        clamp.set_width_request(min_dialog_width);
        toolbar_view.set_content(Some(&clamp));

        configure_natural_scroll_height(main_page_content.upcast_ref(), 680);
        configure_natural_scroll_height(server_page_content.upcast_ref(), 680);
        configure_natural_scroll_height(sync_page_content.upcast_ref(), 680);

        // ── Navigation Pages Construction ────────────────────────────────────
        let nav_view = adw::NavigationView::new();
        nav_view.set_width_request(min_dialog_width);
        self.imp().nav_view.replace(Some(nav_view.clone()));

        let main_nav_page = adw::NavigationPage::with_tag(&toolbar_view, &window_title_str, "main");
        nav_view.add(&main_nav_page);

        if !is_editing_goa {
            // Server Settings Subpage
            let server_toolbar_view = adw::ToolbarView::new();
            let server_header_bar = adw::HeaderBar::new();
            server_header_bar.set_show_end_title_buttons(false);
            server_toolbar_view.add_top_bar(&server_header_bar);

            let server_clamp = adw::Clamp::builder()
                .maximum_size(520)
                .tightening_threshold(440)
                .child(&server_page_content)
                .build();
            server_clamp.set_width_request(min_dialog_width);

            let server_scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .propagate_natural_height(true)
                .vexpand(true)
                .child(&server_clamp)
                .build();
            server_toolbar_view.set_content(Some(&server_scrolled));

            let server_nav_page = adw::NavigationPage::with_tag(&server_toolbar_view, "Server Settings", "server");
            nav_view.add(&server_nav_page);

            // Synchronization Subpage
            let sync_toolbar_view = adw::ToolbarView::new();
            let sync_header_bar = adw::HeaderBar::new();
            sync_header_bar.set_show_end_title_buttons(false);
            sync_toolbar_view.add_top_bar(&sync_header_bar);

            let sync_clamp = adw::Clamp::builder()
                .maximum_size(520)
                .tightening_threshold(440)
                .child(&sync_page_content)
                .build();
            sync_clamp.set_width_request(min_dialog_width);

            let sync_scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .propagate_natural_height(true)
                .vexpand(true)
                .child(&sync_clamp)
                .build();
            sync_toolbar_view.set_content(Some(&sync_scrolled));

            let sync_nav_page = adw::NavigationPage::with_tag(&sync_toolbar_view, "Synchronization", "sync");
            nav_view.add(&sync_nav_page);
        }

        // Connect navigation row clicks to push pages
        {
            let nav_clone = nav_view.clone();
            row_nav_server.connect_activated(move |_| {
                nav_clone.push_by_tag("server");
            });
        }
        {
            let nav_clone = nav_view.clone();
            row_nav_sync.connect_activated(move |_| {
                nav_clone.push_by_tag("sync");
            });
        }

        toast_overlay.set_width_request(min_dialog_width);
        toast_overlay.set_child(Some(&nav_view));
        self.set_child(Some(&toast_overlay));
        self.set_child(Some(&toast_overlay));
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
        profile.follow_global_sync = false;

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
        assert!(!decoded.follow_global_sync);
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
    fn test_account_sync_interval_mapping() {
        assert_eq!(account_sync_interval_to_index(true, true, 10), 0);
        assert_eq!(account_sync_interval_to_index(true, false, 10), 0);
        assert_eq!(account_sync_interval_to_index(false, false, 10), 1);
        assert_eq!(account_sync_interval_to_index(false, true, 0), 1);
        assert_eq!(account_sync_interval_to_index(false, true, 1), 2);
        assert_eq!(account_sync_interval_to_index(false, true, 5), 3);
        assert_eq!(account_sync_interval_to_index(false, true, 10), 4);
        assert_eq!(account_sync_interval_to_index(false, true, 15), 5);
        assert_eq!(account_sync_interval_to_index(false, true, 30), 6);
        assert_eq!(account_sync_interval_to_index(false, true, 60), 7);

        assert_eq!(index_to_account_sync_interval(0, 15), (true, true, 15));
        assert_eq!(index_to_account_sync_interval(1, 15), (false, false, 15));
        assert_eq!(index_to_account_sync_interval(3, 10), (false, true, 5));
        assert_eq!(index_to_account_sync_interval(7, 10), (false, true, 60));
    }
}
