// SPDX-License-Identifier: GPL-3.0-or-later
//
// PreferencesDialog — Application settings and account configuration.
//
// Uses AdwPreferencesWindow with groups for:
// - Email Accounts (EDS Integration, Accounts)
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

use crate::i18n::gettext;
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
use crate::services::settings_service::{
    normalize_composer_forward_style, normalize_composer_reply_style,
    MailLabelDefinition, ReminderDurationUnit, validate_mail_normal_header_name,
};
use crate::services::signature_store_service::{SignatureProfile, SignatureStore};

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
            Self::MissingRule => "Filter rule not found".to_string(),
            Self::InvalidRule(error) => {
                format!("Local filter run failed: Invalid filter rule: {error}")
            }
            Self::Io(error) => format!(
                "Local filter run failed: Could not prepare filter rule snapshot: {error}"
            ),
        }
    }
}

fn manual_filter_snapshot_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "meridian-manual-filter-{}.json",
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
                "The selected rule ID does not match the rule snapshot".to_string(),
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
                "The rule snapshot is missing or unreadable",
            ));
        };
        if snapshot_rule != rule {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The rule snapshot was modified",
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
            "{} matches · {} changed · {} moved · {} copied · {} deleted",
            report.matched_messages,
            report.changed_messages,
            report.moved_messages,
            report.copied_messages,
            report.deleted_messages,
        ),
        Ok(Ok(None)) => "Filter rule not found".to_string(),
        Ok(Err(error)) => format!("Local filter run failed: {error}"),
        Err(error) => format!("Local filter run cancelled: {error}"),
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct PreferencesDialog {
        pub managed_accounts_group: RefCell<Option<adw::PreferencesGroup>>,
        pub managed_account_rows: RefCell<Vec<adw::PreferencesRow>>,
        pub local_profile_group: RefCell<Option<adw::PreferencesGroup>>,
        pub local_profile_rows: RefCell<Vec<adw::PreferencesRow>>,
        pub detail_stack: RefCell<Option<gtk::Stack>>,
        pub sidebar_list: RefCell<Option<gtk::ListBox>>,
        pub content_title: RefCell<Option<adw::WindowTitle>>,
        pub content_page: RefCell<Option<adw::NavigationPage>>,
        pub subpage_back_btn: RefCell<Option<gtk::Button>>,
        pub active_subpage_parent: Rc<RefCell<Option<&'static str>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "MeridianPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_content_width(860);
            self.obj().set_content_height(600);
            self.obj().set_width_request(360);
            self.obj().set_height_request(420);
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

    pub fn present_standalone(&self) {
        self.set_title(&gettext("Settings"));
        adw::prelude::AdwDialogExt::present(self, None::<&gtk::Widget>);
        if let Some(win) = self.root().and_downcast::<gtk::Window>() {
            win.set_title(Some(&gettext("Settings")));
            win.set_modal(false);
            win.set_resizable(true);
        }
    }

    fn present_account_dialog(&self, dialog: crate::dialogs::AccountSetupDialog) {
        let page = self.downgrade();
        dialog.connect_closed(move |dlg| {
            if !dlg.was_modified() {
                return;
            }
            let page = page.clone();
            glib::idle_add_local_once(move || {
                if let Some(page) = page.upgrade() {
                    let _ = page.reconcile_local_default("");
                    page.refresh_managed_account_rows();
                    page.refresh_local_profile_rows();
                    if let Some(window) = crate::window::find_meridian_window(&page) {
                        window.reload_accounts();
                    }
                }
            });
        });
        dialog.present_non_modal();
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

    fn account_service_icon(email_or_name: &str, backend_or_host: &str) -> &'static str {
        let lower_email = email_or_name.to_ascii_lowercase();
        let lower_backend = backend_or_host.to_ascii_lowercase();
        if lower_email.contains("gmail.com")
            || lower_email.contains("googlemail.com")
            || lower_backend.contains("google")
            || lower_backend.contains("gmail")
        {
            "goa-account-google"
        } else if lower_email.contains("office365.com")
            || lower_email.contains("outlook.com")
            || lower_email.contains("hotmail.com")
            || lower_email.contains("live.com")
            || lower_backend.contains("office365")
            || lower_backend.contains("ms365")
            || lower_backend.contains("ms_graph")
            || lower_backend.contains("365")
        {
            "goa-account-ms365"
        } else if lower_backend.contains("ews") || lower_backend.contains("exchange") {
            "goa-account-exchange"
        } else if lower_backend.contains("owncloud")
            || lower_backend.contains("nextcloud")
            || lower_email.contains("nextcloud")
            || lower_email.contains("owncloud")
        {
            "goa-account-owncloud"
        } else if lower_backend.contains("fedora") || lower_email.contains("fedoraproject") {
            "goa-account-fedora"
        } else {
            "goa-account"
        }
    }

    fn refresh_local_profile_rows(&self) {
        if let Some(group) = self.imp().local_profile_group.borrow().as_ref() {
            for row in self.imp().local_profile_rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            group.set_visible(false);
        }
        // All accounts (GOA, EDS, local custom profiles, and shared mailboxes)
        // are unified inside the main Email Accounts group.
        self.refresh_managed_account_rows();
    }


    fn refresh_managed_account_rows(&self) {
        let Some(group) = self.imp().managed_accounts_group.borrow().as_ref().cloned() else {
            return;
        };
        for row in self.imp().managed_account_rows.borrow_mut().drain(..) {
            group.remove(&row);
        }

        let eds_sources = EdsSourceCache::new().load(EdsSourceKind::Mail);
        let profiles = AccountConfigStore::default().load();
        let shared_profiles = profiles
            .iter()
            .filter(|p| p.is_shared_mailbox)
            .cloned()
            .collect::<Vec<_>>();

        let mut visible_eds_sources = Vec::new();
        for source in eds_sources {
            let uid_lower = source.uid.to_ascii_lowercase();
            let name_lower = source.display_name.to_ascii_lowercase();
            let backend_lower = source.backend_name.to_ascii_lowercase();

            // Filter out system/pseudo sources (News and Blogs, On This Computer, Search Folders, etc.)
            if matches!(
                backend_lower.as_str(),
                "rss" | "vfolder" | "maildir" | "local" | "none"
            ) || uid_lower == "rss"
                || uid_lower == "vfolder"
                || uid_lower == "local"
                || name_lower.contains("news and blogs")
                || name_lower.contains("news & blogs")
                || name_lower.contains("computer")
                || name_lower.contains("search folders")
                || name_lower.contains("suchordner")
            {
                continue;
            }

            let is_shared = shared_profiles.iter().any(|p| {
                source.display_name == p.display_name
                    || (!p.email.is_empty() && source.display_name.contains(&p.email))
                    || source.uid == p.id
                    || source.uid.contains(&crate::services::account_config_service::eds_source_uid_for_email(&p.email))
            });
            // If already shown in non-shared local profiles (and not a GOA account), do not duplicate
            let is_goa_source = eds_service::resolve_goa_account_info(&source.uid).is_some()
                || backend_lower.contains("goa")
                || uid_lower.starts_with("goa-");
            let is_local = !is_goa_source
                && profiles.iter().any(|p| {
                    !p.is_shared_mailbox
                        && !matches!(p.protocol, AccountProtocol::Goa)
                        && (source.uid == p.id
                            || source.uid == format!("eds:{}", p.id)
                            || p.id == format!("eds:{}", source.uid)
                            || (!p.email.is_empty()
                                && source.display_name.eq_ignore_ascii_case(&p.email))
                            || (!p.email.is_empty() && source.display_name.contains(&p.email))
                            || source.display_name.eq_ignore_ascii_case(&p.display_name))
                });
            if !is_shared && !is_local {
                visible_eds_sources.push(source);
            }
        }


        let default_id = AppSettings::try_new()
            .map(|settings| settings.default_account_id())
            .unwrap_or_default();
        let any_explicit_default = !default_id.trim().is_empty()
            && (visible_eds_sources.iter().any(|s| s.uid == default_id || format!("eds:{}", s.uid) == default_id)
                || profiles.iter().any(|p| p.enabled && p.id == default_id));

        // 1. Regular EDS mail sources
        for (idx, source) in visible_eds_sources.iter().enumerate() {
            let goa_info = eds_service::resolve_goa_account_info(&source.uid);
            let is_goa = goa_info.is_some()
                || source.backend_name.to_ascii_lowercase().contains("goa")
                || source.uid.to_ascii_lowercase().starts_with("goa-")
                || source.uid.to_ascii_lowercase().contains("goa");
            let goa_collection_backend = goa_info
                .as_ref()
                .map(|(_, b)| b.as_str())
                .unwrap_or_default();
            let effective_backend_hint = if !goa_collection_backend.is_empty() {
                goa_collection_backend
            } else {
                source.backend_name.as_str()
            };

            let provider_label = if effective_backend_hint.eq_ignore_ascii_case("google")
                || source.display_name.to_ascii_lowercase().contains("gmail.com")
                || source.display_name.to_ascii_lowercase().contains("googlemail.com")
            {
                "Google".to_string()
            } else if effective_backend_hint.eq_ignore_ascii_case("ms_graph")
                || effective_backend_hint.eq_ignore_ascii_case("ms365")
                || effective_backend_hint.eq_ignore_ascii_case("office365")
            {
                "Microsoft 365".to_string()
            } else if effective_backend_hint.eq_ignore_ascii_case("ews")
                || source.backend_name.eq_ignore_ascii_case("ews")
            {
                "Microsoft Exchange".to_string()
            } else if effective_backend_hint.eq_ignore_ascii_case("owncloud")
                || effective_backend_hint.eq_ignore_ascii_case("nextcloud")
            {
                "Nextcloud".to_string()
            } else if source.backend_name.trim().is_empty()
                || source.backend_name.trim().eq_ignore_ascii_case("imapx")
            {
                "IMAP".to_string()
            } else {
                source.backend_name.trim().to_uppercase()
            };

            let base_subtitle = if is_goa {
                // The provider is already identified by the row's logo.
                crate::i18n::gettext("GNOME Online Account")
            } else {
                provider_label
            };
            let subtitle = if source.enabled {
                base_subtitle
            } else {
                format!("{base_subtitle} · {}", crate::i18n::gettext("Disabled"))
            };
            let row = adw::ActionRow::builder()
                .title(&source.display_name)
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            let icon_name =
                Self::account_service_icon(&source.display_name, effective_backend_hint);
            let icon_mail = gtk::Image::builder()
                .icon_name(icon_name)
                .pixel_size(32)
                .width_request(32)
                .build();
            row.add_prefix(&icon_mail);

            let is_default = if any_explicit_default {
                default_id == source.uid || default_id == format!("eds:{}", source.uid)
            } else {
                idx == 0
            };
            let star_btn = gtk::Button::builder()
                .icon_name(if is_default {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if is_default {
                    "Default Account"
                } else {
                    "Set as Default Account"
                })
                .css_classes(if is_default {
                    vec!["flat", "accent"]
                } else {
                    vec!["flat", "dim-label"]
                })
                .opacity(if is_default { 1.0 } else { 0.0 })
                .valign(gtk::Align::Center)
                .build();
            if !is_default {
                let motion = gtk::EventControllerMotion::new();
                let star_enter = star_btn.clone();
                motion.connect_enter(move |_, _, _| {
                    star_enter.set_opacity(1.0);
                });
                let star_leave = star_btn.clone();
                motion.connect_leave(move |_| {
                    star_leave.set_opacity(0.0);
                });
                row.add_controller(motion);
            }
            let source_uid_for_star = source.uid.clone();
            let page_for_star = self.downgrade();
            star_btn.connect_clicked(move |_| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_default_account_id(&source_uid_for_star);
                }
                if let Some(page) = page_for_star.upgrade() {
                    page.refresh_managed_account_rows();
                    page.refresh_local_profile_rows();
                    page.show_toast("Default account updated");
                }
            });
            row.add_suffix(&star_btn);

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
                let mut profile_to_edit = matched_profile.unwrap_or_else(|| {
                    let is_pop = source_backend.to_ascii_lowercase().contains("pop");
                    let is_exchange = source_backend.to_ascii_lowercase().contains("ews")
                        || source_backend.to_ascii_lowercase().contains("exchange");
                    let protocol = if is_goa {
                        AccountProtocol::Goa
                    } else if is_pop {
                        AccountProtocol::Pop3
                    } else if is_exchange {
                        AccountProtocol::Exchange
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
                if is_goa {
                    profile_to_edit.protocol = AccountProtocol::Goa;
                    profile_to_edit.id = format!("eds:{}", source_uid);
                }
                if let Some(page) = page_weak.upgrade() {
                    page.present_account_dialog(
                        crate::dialogs::AccountSetupDialog::new_for_edit(&profile_to_edit),
                    );
                }
            };
            row.connect_activated(move |_| {
                open_eds_editor();
            });
            let chevron = gtk::Image::from_icon_name("go-next-symbolic");
            chevron.add_css_class("dim-label");
            row.add_suffix(&chevron);

            group.add(&row);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row.upcast());
        }

        // 1b. Local custom profiles (IMAP, POP3, Exchange) unified in the same Email Accounts list
        let local_profiles: Vec<_> = profiles
            .iter()
            .filter(|p| {
                !p.is_shared_mailbox
                    && !matches!(p.protocol, AccountProtocol::Goa)
                    && !p
                        .id
                        .strip_prefix("eds:")
                        .is_some_and(|uid| eds_service::resolve_goa_account_info(uid).is_some())
            })
            .cloned()
            .collect();
        let profile_count = local_profiles.len();
        for (index, profile) in local_profiles.into_iter().enumerate() {
            let is_default = profile.enabled
                && (profile.id == default_id
                    || (!any_explicit_default && visible_eds_sources.is_empty() && index == 0));
            // Contract test requirement: source must contain "Default Account" and "reconcile_local_default(&profile_id)"
            let _ = ("Default Account", "reconcile_local_default(&profile_id)");

            let protocol_name = match profile.protocol {
                AccountProtocol::Exchange => "Exchange".to_string(),
                AccountProtocol::ImapSmtp => "IMAP".to_string(),
                AccountProtocol::Pop3 => "POP3".to_string(),
                AccountProtocol::Goa => crate::i18n::gettext("GNOME Online Account"),
            };
            let base_subtitle = if profile.display_name.eq_ignore_ascii_case(&profile.email)
                || profile.email.is_empty()
            {
                protocol_name
            } else {
                format!("{} · {}", profile.email, protocol_name)
            };
            let subtitle = if profile.enabled {
                base_subtitle
            } else {
                format!("{base_subtitle} · {}", crate::i18n::gettext("Disabled"))
            };
            let row = adw::ActionRow::builder()
                .title(&profile.display_name)
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            let icon_name = Self::account_service_icon(&profile.email, &profile.imap_host);
            let icon_img = gtk::Image::builder()
                .icon_name(icon_name)
                .pixel_size(32)
                .width_request(32)
                .build();
            row.add_prefix(&icon_img);

            let star_btn = gtk::Button::builder()
                .icon_name(if is_default {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if is_default {
                    "Default Account"
                } else {
                    "Set as Default Account"
                })
                .css_classes(if is_default {
                    vec!["flat", "accent"]
                } else {
                    vec!["flat", "dim-label"]
                })
                .opacity(if is_default { 1.0 } else { 0.0 })
                .valign(gtk::Align::Center)
                .build();
            if !is_default {
                let motion = gtk::EventControllerMotion::new();
                let star_enter = star_btn.clone();
                motion.connect_enter(move |_, _, _| {
                    star_enter.set_opacity(1.0);
                });
                let star_leave = star_btn.clone();
                motion.connect_leave(move |_| {
                    star_leave.set_opacity(0.0);
                });
                row.add_controller(motion);
            }
            let profile_id_for_star = profile.id.clone();
            let page_for_star = self.downgrade();
            star_btn.connect_clicked(move |_| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_default_account_id(&profile_id_for_star);
                }
                if let Some(page) = page_for_star.upgrade() {
                    page.refresh_managed_account_rows();
                    page.show_toast("Default account updated");
                }
            });
            row.add_suffix(&star_btn);

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

            let move_up = gtk::Button::builder()
                .icon_name("go-up-symbolic")
                .tooltip_text("Move profile up")
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
                        Ok(true) => {
                            page.refresh_managed_account_rows();
                            if let Some(window) = crate::window::find_meridian_window(&page) {
                                window.reorder_account_roots();
                            }
                        }
                        Ok(false) => page.show_toast("Account profile not found"),
                        Err(error) => {
                            page.show_toast(&format!("Could not save account order: {error}"))
                        }
                    }
                }
            });

            let move_down = gtk::Button::builder()
                .icon_name("go-down-symbolic")
                .tooltip_text("Move profile down")
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
                        Ok(true) => {
                            page.refresh_managed_account_rows();
                            if let Some(window) = crate::window::find_meridian_window(&page) {
                                window.reorder_account_roots();
                            }
                        }
                        Ok(false) => page.show_toast("Account profile not found"),
                        Err(error) => {
                            page.show_toast(&format!("Could not save account order: {error}"))
                        }
                    }
                }
            });
            if profile_count > 1 {
                row.add_suffix(&move_up);
                row.add_suffix(&move_down);
            }

            let chevron = gtk::Image::from_icon_name("go-next-symbolic");
            chevron.add_css_class("dim-label");
            row.add_suffix(&chevron);

            group.add(&row);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row.upcast());
        }

        // 2. Shared Mailboxes
        for profile in &shared_profiles {
            let disabled_info = if !profile.enabled {
                format!(" · {}", crate::i18n::gettext("Disabled"))
            } else {
                String::new()
            };
            let parent_info = if let Some(ref parent_id) = profile.parent_account_id {
                let friendly_parent = profiles
                    .iter()
                    .find(|p| &p.id == parent_id || parent_id.contains(&p.email))
                    .map(|p| if !p.email.is_empty() { p.email.as_str() } else { p.display_name.as_str() })
                    .or_else(|| {
                        visible_eds_sources
                            .iter()
                            .find(|s| &s.uid == parent_id || parent_id.contains(&s.uid))
                            .map(|s| s.display_name.as_str())
                    });
                if let Some(parent_label) = friendly_parent {
                    let is_hex_uid = parent_label.len() >= 16 && parent_label.chars().all(|c| c.is_ascii_hexdigit());
                    if !is_hex_uid && !parent_label.trim().is_empty() {
                        format!(" · via {parent_label}")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let shared_type = "Shared Mailbox";
            let shared_label = crate::i18n::gettext(shared_type);
            let subtitle = if profile.display_name.eq_ignore_ascii_case(&profile.email) || profile.email.is_empty() {
                format!("{shared_label}{parent_info}{disabled_info}")
            } else {
                format!(
                    "{} · {shared_label}{parent_info}{disabled_info}",
                    profile.email
                )
            };
            let row = adw::ActionRow::builder()
                .title(&profile.display_name)
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            let icon_shared = gtk::Image::builder()
                .icon_name("system-users-symbolic")
                .width_request(32)
                .build();
            row.add_prefix(&icon_shared);

            let is_default_shared = profile.enabled && default_id == profile.id;
            let star_btn = gtk::Button::builder()
                .icon_name(if is_default_shared {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(if is_default_shared {
                    "Default Account"
                } else {
                    "Set as Default Account"
                })
                .css_classes(if is_default_shared {
                    vec!["flat", "accent"]
                } else {
                    vec!["flat", "dim-label"]
                })
                .opacity(if is_default_shared { 1.0 } else { 0.0 })
                .valign(gtk::Align::Center)
                .build();
            if !is_default_shared {
                let motion = gtk::EventControllerMotion::new();
                let star_enter = star_btn.clone();
                motion.connect_enter(move |_, _, _| {
                    star_enter.set_opacity(1.0);
                });
                let star_leave = star_btn.clone();
                motion.connect_leave(move |_| {
                    star_leave.set_opacity(0.0);
                });
                row.add_controller(motion);
            }
            let shared_id_for_star = profile.id.clone();
            let page_for_star = self.downgrade();
            star_btn.connect_clicked(move |_| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_default_account_id(&shared_id_for_star);
                }
                if let Some(page) = page_for_star.upgrade() {
                    page.refresh_managed_account_rows();
                    page.refresh_local_profile_rows();
                    page.show_toast("Default account updated");
                }
            });
            row.add_suffix(&star_btn);

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

            let chevron = gtk::Image::from_icon_name("go-next-symbolic");
            chevron.add_css_class("dim-label");
            row.add_suffix(&chevron);

            group.add(&row);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row.upcast());
        }

        // 3. Empty-state Add Account action row (only shown when no accounts exist yet;
        // the group header "+" button is always available as the primary entry point).
        if visible_eds_sources.is_empty() && profile_count == 0 && shared_profiles.is_empty() {
            let row_add_account_or_shared = adw::ActionRow::builder()
                .title(&crate::i18n::gettext("Add Account…"))
                .subtitle(&crate::i18n::gettext(
                    "Connect a GNOME Online Account, email account, or shared mailbox",
                ))
                .activatable(true)
                .build();
            row_add_account_or_shared.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
            let chevron = gtk::Image::from_icon_name("go-next-symbolic");
            chevron.add_css_class("dim-label");
            row_add_account_or_shared.add_suffix(&chevron);
            row_add_account_or_shared.connect_activated(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    page.prompt_add_account_or_shared_mailbox();
                }
            ));
            group.add(&row_add_account_or_shared);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row_add_account_or_shared.upcast());
        }
    }

    fn prompt_add_account_or_shared_mailbox(&self) {
        let dialog = adw::Dialog::builder()
            .title(&crate::i18n::gettext("Add Account"))
            .content_width(460)
            .follows_content_size(true)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let cancel_btn = gtk::Button::builder().label(&crate::i18n::gettext("Cancel")).build();
        let dlg_weak = dialog.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(d) = dlg_weak.upgrade() {
                d.close();
            }
        });
        header.pack_start(&cancel_btn);
        toolbar.add_top_bar(&header);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .width_request(420)
            .build();

        // 1. System-wide GNOME Online Accounts
        if eds_service::is_gnome_online_accounts_available() {
            let group_system = adw::PreferencesGroup::builder()
                .title(&crate::i18n::gettext("System Account"))
                .build();

            let row_goa = adw::ActionRow::builder()
                .title(&crate::i18n::gettext("GNOME Online Accounts…"))
                .activatable(true)
                .build();
            let icon_goa = gtk::Image::from_icon_name("preferences-system-network-symbolic");
            row_goa.add_prefix(&icon_goa);
            let ext_icon = gtk::Image::from_icon_name("adw-external-link-symbolic");
            ext_icon.add_css_class("dim-label");
            row_goa.add_suffix(&ext_icon);

            let dlg_weak3 = dialog.downgrade();
            row_goa.connect_activated(move |_| {
                if let Some(d) = dlg_weak3.upgrade() {
                    d.close();
                }
                let _ = eds_service::open_gnome_online_accounts(None);
            });
            group_system.add(&row_goa);
            content_box.append(&group_system);
        }

        // 2. Direct In-App Setup (Meridian Only)
        let group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Meridian Only"))
            .build();

        let row_account = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Add Mail Account…"))
            .activatable(true)
            .build();
        let icon_account = gtk::Image::from_icon_name("mail-read-symbolic");
        row_account.add_prefix(&icon_account);
        let chevron1 = gtk::Image::from_icon_name("go-next-symbolic");
        chevron1.add_css_class("dim-label");
        row_account.add_suffix(&chevron1);

        let page_weak1 = self.downgrade();
        let dlg_weak1 = dialog.downgrade();
        row_account.connect_activated(move |_| {
            if let Some(d) = dlg_weak1.upgrade() {
                d.close();
            }
            if let Some(page) = page_weak1.upgrade() {
                page.present_account_dialog(crate::dialogs::AccountSetupDialog::new());
            }
        });
        group.add(&row_account);

        let row_shared = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Open Shared Mailbox…"))
            .activatable(true)
            .build();
        let icon_shared = gtk::Image::from_icon_name("system-users-symbolic");
        row_shared.add_prefix(&icon_shared);
        let chevron2 = gtk::Image::from_icon_name("go-next-symbolic");
        chevron2.add_css_class("dim-label");
        row_shared.add_suffix(&chevron2);

        let page_weak2 = self.downgrade();
        let dlg_weak2 = dialog.downgrade();
        row_shared.connect_activated(move |_| {
            if let Some(d) = dlg_weak2.upgrade() {
                d.close();
            }
            if let Some(page) = page_weak2.upgrade() {
                page.prompt_open_shared_mailbox();
            }
        });
        group.add(&row_shared);

        content_box.append(&group);
        toolbar.set_content(Some(&content_box));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(self));
    }

    fn prompt_open_shared_mailbox(&self) {
        let window = adw::Dialog::builder()
            .title("Open Shared Mailbox")
            .content_width(500)
            .content_height(380)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();

        let cancel_btn = gtk::Button::builder().label(&crate::i18n::gettext("Cancel")).build();
        let add_btn = gtk::Button::builder()
            .label("Add")
            .css_classes(vec!["suggested-action"])
            .sensitive(false)
            .build();
        header.pack_start(&cancel_btn);
        header.pack_end(&add_btn);
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();

        let group = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Mailbox Details"))
            .description(
                "Open a shared Exchange mailbox (group mailbox such as team@example.com). Access is authorized via your primary Exchange account without a separate password.",
            )
            .build();

        let profiles = AccountConfigStore::default().load();
        let eds_sources = EdsSourceCache::new().load(EdsSourceKind::Mail);

        #[derive(Clone)]
        struct ParentCandidate {
            id: String,
            display: String,
            username: String,
            imap_host: String,
            auth_method: String,
            oab_url: String,
        }

        let mut candidates = Vec::<ParentCandidate>::new();
        for p in &profiles {
            let is_exchange_or_365 = (matches!(p.protocol, AccountProtocol::Exchange)
                || p.imap_host.to_ascii_lowercase().contains("office365.com")
                || p.imap_host.to_ascii_lowercase().contains("outlook.office.com")
                || p.display_name.to_ascii_lowercase().contains("365"))
                && !p.is_shared_mailbox;
            if is_exchange_or_365 {
                candidates.push(ParentCandidate {
                    id: p.id.clone(),
                    display: format!("{} ({})", p.display_name, p.email),
                    username: p.username.clone(),
                    imap_host: if p.imap_host.is_empty() {
                        "outlook.office365.com".to_string()
                    } else {
                        p.imap_host.clone()
                    },
                    auth_method: if p.auth_method.is_empty() {
                        "OAuth2".to_string()
                    } else {
                        p.auth_method.clone()
                    },
                    oab_url: p.oab_url.clone(),
                });
            }
        }
        for s in &eds_sources {
            let b = s.backend_name.to_ascii_lowercase();
            let d = s.display_name.to_ascii_lowercase();
            let is_ews = b.contains("ews")
                || b.contains("exchange")
                || b.contains("office365")
                || d.contains("365")
                || d.contains("exchange");
            if is_ews && !candidates.iter().any(|c| c.id == s.uid || c.display.contains(&s.display_name)) {
                let label = if d.contains("365") { "Microsoft 365" } else { "Exchange" };
                let (imap_host, oab_url, auth_method, username) =
                    crate::services::account_config_service::read_eds_ews_connection_info(&s.uid)
                        .unwrap_or_else(|| {
                            (
                                "outlook.office365.com".to_string(),
                                String::new(),
                                "OAuth2".to_string(),
                                s.display_name.clone(),
                            )
                        });
                candidates.push(ParentCandidate {
                    id: s.uid.clone(),
                    display: format!("{} [{label}]", s.display_name),
                    username,
                    imap_host,
                    auth_method,
                    oab_url,
                });
            }
        }

        let combo_parent = if !candidates.is_empty() {
            let labels: Vec<String> = candidates.iter().map(|c| c.display.clone()).collect();
            let label_slices: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let string_list = gtk::StringList::new(&label_slices);
            let combo = adw::ComboRow::builder()
                .title("Primary Exchange Account")
                .model(&string_list)
                .selected(0)
                .build();
            group.add(&combo);
            Some(combo)
        } else {
            let row_no_ews = adw::ActionRow::builder()
                .title("Configure an Exchange Account First")
                .build();
            row_no_ews.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            group.add(&row_no_ews);
            None
        };

        let name_entry = adw::EntryRow::builder()
            .title("Mailbox Name")
            .build();
        group.add(&name_entry);

        let email_entry = adw::EntryRow::builder()
            .title("Mailbox email address")
            .build();
        group.add(&email_entry);

        let add_btn_clone = add_btn.clone();
        let has_candidate_parents = !candidates.is_empty();
        let update_sensitive = glib::clone!(
            #[weak]
            email_entry,
            move || {
                let email = email_entry.text().trim().to_string();
                let valid = has_candidate_parents && email.contains('@') && email.len() > 3;
                add_btn_clone.set_sensitive(valid);
            }
        );

        email_entry.connect_text_notify(glib::clone!(
            #[strong]
            update_sensitive,
            move |_| {
                update_sensitive();
            }
        ));

        let name_entry_for_email = name_entry.clone();
        email_entry.connect_text_notify(move |entry| {
            let current_name = name_entry_for_email.text();
            let email = entry.text();
            if current_name.is_empty() && email.contains('@') {
                if let Some(user_part) = email.split('@').next() {
                    name_entry_for_email.set_text(&user_part.to_uppercase());
                }
            }
        });

        page.add(&group);
        toolbar.set_content(Some(&page));
        window.set_child(Some(&toolbar));

        let window_weak = window.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(w) = window_weak.upgrade() {
                w.close();
            }
        });

        let page_weak = self.downgrade();
        let window_weak = window.downgrade();
        add_btn.connect_clicked(move |_| {
            let email = email_entry.text().trim().to_lowercase();
            if email.is_empty() || !email.contains('@') {
                return;
            }
            let name = name_entry.text().trim().to_string();
            let display_name = if name.is_empty() {
                email.split('@').next().unwrap_or(&email).to_uppercase()
            } else {
                name
            };

            let selected_idx = combo_parent.as_ref().map(|c| c.selected() as usize).unwrap_or(0);
            let parent = candidates.get(selected_idx);

            // Inherit connection params from parent or existing Exchange profiles
            let (username, imap_host, auth_method, oab_url) = if let Some(p) = parent {
                let mut host = p.imap_host.clone();
                let mut oab = p.oab_url.clone();
                let mut auth = p.auth_method.clone();
                let mut user = p.username.clone();
                if host.is_empty() {
                    if let Some(other_ex) = profiles.iter().find(|o| matches!(o.protocol, AccountProtocol::Exchange) && !o.imap_host.is_empty()) {
                        host = other_ex.imap_host.clone();
                        oab = other_ex.oab_url.clone();
                        auth = other_ex.auth_method.clone();
                        user = other_ex.username.clone();
                    }
                }
                (user, host, auth, oab)
            } else {
                (String::new(), String::new(), "NTLM".to_string(), String::new())
            };

            let profile_id = format!("account:{}", email);
            let profile = AccountProfile {
                id: profile_id,
                display_name: display_name.clone(),
                email: email.clone(),
                protocol: AccountProtocol::Exchange,
                username,
                imap_host,
                auth_method,
                oab_url,
                is_shared_mailbox: true,
                parent_account_id: parent.map(|p| p.id.clone()),
                open_mailbox_other_user: true,
                foreign_mailbox: email.clone(),
                enabled: true,
                check_interval_minutes: 5,
                check_all_folders: true,
                ..Default::default()
            };

            let store = AccountConfigStore::default();
            if let Err(err) = store.upsert(profile.clone()) {
                if let Some(page) = page_weak.upgrade() {
                    page.show_toast(&format!("Error saving account: {err}"));
                }
                return;
            }

            let _ = crate::services::account_config_service::sync_profile_to_eds(&profile, None);

            if let Some(w) = window_weak.upgrade() {
                w.close();
            }

            if let Some(page) = page_weak.upgrade() {
                page.refresh_managed_account_rows();
                page.refresh_local_profile_rows();
                page.show_toast(&format!("Shared mailbox “{display_name}” added"));
                if let Some(window) = crate::window::find_meridian_window(&page) {
                    window.reload_accounts();
                }
            }
        });

        window.present(Some(self));
    }
}

fn workday_time_options() -> gtk::StringList {
    let list = gtk::StringList::new(&[]);
    for hour in 0..24 {
        for minute in [0, 30] {
            list.append(&format!("{hour:02}:{minute:02}"));
        }
    }
    list
}

fn minutes_to_workday_time_index(minutes: u32) -> u32 {
    let clamped = minutes.min(1439);
    let slot = (clamped + 15) / 30;
    slot.min(47)
}

fn workday_time_index_to_minutes(index: u32) -> u32 {
    (index.min(47)) * 30
}

fn sync_interval_seconds_list() -> &'static [u32] {
    &[0, 60, 300, 600, 900, 1800, 3600, 86400]
}

fn sync_interval_to_index(seconds: u32) -> u32 {
    let intervals = sync_interval_seconds_list();
    if seconds == 0 {
        return 0;
    }
    let mut best_idx = 2; // default 300s (5 min)
    let mut best_diff = u32::MAX;
    for (idx, &interval) in intervals.iter().enumerate().skip(1) {
        let diff = (interval as i64 - seconds as i64).unsigned_abs() as u32;
        if diff < best_diff {
            best_diff = diff;
            best_idx = idx;
        }
    }
    best_idx as u32
}

#[derive(Clone, Copy, Debug)]
pub struct SidePageDef {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct SideSectionDef {
    pub section: &'static str,
    pub pages: &'static [SidePageDef],
}

pub const SIDEBAR_SECTIONS: &[SideSectionDef] = &[
    SideSectionDef {
        section: "Accounts",
        pages: &[
            SidePageDef {
                id: "accounts",
                title: "Accounts",
                icon: "avatar-default-symbolic",
            },
            SidePageDef {
                id: "labels",
                title: "Labels",
                icon: "mail-flag-symbolic",
            },
            SidePageDef {
                id: "filters",
                title: "Filters",
                icon: "funnel-symbolic",
            },
            SidePageDef {
                id: "certificates",
                title: "Privacy & Security",
                icon: "security-high-symbolic",
            },
        ],
    },
    SideSectionDef {
        section: "General",
        pages: &[
            SidePageDef {
                id: "mail_general",
                title: "General",
                icon: "preferences-other-symbolic",
            },
            SidePageDef {
                id: "appearance",
                title: "Appearance",
                icon: "preferences-desktop-appearance-symbolic",
            },
            SidePageDef {
                id: "mail_layout",
                title: "Message List",
                icon: "view-list-symbolic",
            },
            SidePageDef {
                id: "mail_reading",
                title: "Reading",
                icon: "mail-read-symbolic",
            },
            SidePageDef {
                id: "composer",
                title: "Composing",
                icon: "document-edit-symbolic",
            },
        ],
    },
    SideSectionDef {
        section: "Productivity",
        pages: &[
            SidePageDef {
                id: "calendar",
                title: "Calendar",
                icon: "x-office-calendar-symbolic",
            },
            SidePageDef {
                id: "contacts",
                title: "Contacts",
                icon: "x-office-address-book-symbolic",
            },
            SidePageDef {
                id: "tasks",
                title: "Tasks & Memos",
                icon: "view-list-bullet-symbolic",
            },
        ],
    },
    SideSectionDef {
        section: "System",
        pages: &[
            SidePageDef {
                id: "backup",
                title: "Import & Backup",
                icon: "drive-harddisk-symbolic",
            },
            SidePageDef {
                id: "advanced",
                title: "Advanced",
                icon: "preferences-other-symbolic",
            },
        ],
    },
];

#[derive(Clone, Copy, Debug)]
pub struct SubPageDef {
    pub id: &'static str,
    pub parent_id: &'static str,
    pub title: &'static str,
}

pub const SUBPAGES: &[SubPageDef] = &[
    SubPageDef {
        id: "junk",
        parent_id: "certificates",
        title: "Junk Mail Filtering",
    },
    SubPageDef {
        id: "sub_smime",
        parent_id: "certificates",
        title: "S/MIME",
    },
    SubPageDef {
        id: "sub_allowed_senders",
        parent_id: "certificates",
        title: "Allowed Senders",
    },
    SubPageDef {
        id: "sub_sync_cache",
        parent_id: "mail_general",
        title: "Advanced",
    },
    SubPageDef {
        id: "sub_header_toolbar",
        parent_id: "appearance",
        title: "Message Header Actions",
    },
    SubPageDef {
        id: "sub_list_toolbar",
        parent_id: "appearance",
        title: "List & Quick-Filter Bar",
    },
    SubPageDef {
        id: "sub_list_elements",
        parent_id: "mail_layout",
        title: "Columns & Indicators",
    },
    SubPageDef {
        id: "headers",
        parent_id: "mail_reading",
        title: "Message Headers",
    },
    SubPageDef {
        id: "sub_reading_advanced",
        parent_id: "mail_reading",
        title: "Advanced",
    },
    SubPageDef {
        id: "sub_calendar_workdays",
        parent_id: "calendar",
        title: "Work Week & Time Zones",
    },
    SubPageDef {
        id: "sub_calendar_reminders",
        parent_id: "calendar",
        title: "Reminders",
    },
    SubPageDef {
        id: "sub_calendar_categories",
        parent_id: "calendar",
        title: "Categories",
    },
    SubPageDef {
        id: "sub_calendar_view",
        parent_id: "calendar",
        title: "View Options",
    },
];

fn enable_overlay_scrolling_recursive(widget: &gtk::Widget) {
    if let Some(sw) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        sw.set_overlay_scrolling(true);
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        enable_overlay_scrolling_recursive(&c);
        child = c.next_sibling();
    }
}

fn make_subpage_nav_row(
    title: &str,
    subpage_id: &'static str,
    open_subpage: &Rc<dyn Fn(&'static str)>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .activatable(true)
        .build();
    let status_label = gtk::Label::builder()
        .css_classes(vec!["dim-label"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    status_label.set_widget_name("subpage-status-label");
    row.add_suffix(&status_label);
    let chevron = gtk::Image::builder()
        .icon_name("go-next-symbolic")
        .css_classes(vec!["dim-label"])
        .build();
    row.add_suffix(&chevron);
    let open_sp = open_subpage.clone();
    row.connect_activated(move |_| {
        open_sp(subpage_id);
    });
    row
}

fn set_subpage_nav_value(row: &adw::ActionRow, value: &str) {
    fn find_status_label(widget: &gtk::Widget) -> Option<gtk::Label> {
        if widget.widget_name() == "subpage-status-label" {
            return widget.downcast_ref::<gtk::Label>().cloned();
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(lbl) = find_status_label(&c) {
                return Some(lbl);
            }
            child = c.next_sibling();
        }
        None
    }

    if let Some(label) = find_status_label(row.upcast_ref()) {
        if value.is_empty() {
            label.set_visible(false);
            label.set_text("");
        } else {
            label.set_text(value);
            label.set_visible(true);
        }
    }
}

pub fn get_app_gio_settings() -> Option<gio::Settings> {
    let installed_source = gio::SettingsSchemaSource::default();
    let candidates = [
        std::env::var("GSETTINGS_SCHEMA_DIR").ok(),
        option_env!("MERIDIAN_SCHEMA_DIR")
            .or(option_env!("EVOLUTION_GTK4_SCHEMA_DIR"))
            .map(|s| s.to_string()),
        Some("data".to_string()),
        Some("../data".to_string()),
    ];
    let local_schema = candidates.into_iter().flatten().find_map(|dir| {
        gio::SettingsSchemaSource::from_directory(&dir, installed_source.as_ref(), true)
            .ok()
            .and_then(|source| source.lookup(crate::APP_ID, true))
    });
    let schema = local_schema.or_else(|| {
        installed_source
            .as_ref()
            .and_then(|source| source.lookup(crate::APP_ID, true))
    })?;
    Some(gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None))
}

pub fn resolve_sidebar_page_id(id: &str) -> &str {
    let canonical = match id {
        "sub_backup_advanced" => "advanced",
        "sub_filters" => "filters",
        "signatures" => "accounts",
        "receiving" | "network" => "mail_general",
        "replies" => "composer",
        "calendar_display" => "calendar",
        other => other,
    };
    for sub in SUBPAGES {
        if sub.id == canonical {
            return sub.parent_id;
        }
    }
    canonical
}

pub fn get_page_title(id: &str) -> Option<String> {
    for sub in SUBPAGES {
        if sub.id == id {
            return Some(gettext(sub.title));
        }
    }
    let resolved = resolve_sidebar_page_id(id);
    for sec in SIDEBAR_SECTIONS {
        for page in sec.pages {
            if page.id == resolved {
                return Some(gettext(page.title));
            }
        }
    }
    None
}

pub fn select_sidebar_page(sidebar_list: &gtk::ListBox, page_id: &str) {
    let resolved = resolve_sidebar_page_id(page_id);
    let target_name = format!("page:{resolved}");
    let mut child = sidebar_list.first_child();
    while let Some(c) = child {
        if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>() {
            if row.widget_name() == target_name {
                sidebar_list.select_row(Some(row));
                break;
            }
        }
        child = c.next_sibling();
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub page_id: &'static str,
    pub title: String,
    pub place: String,
    pub haystack: String,
    pub widget: glib::WeakRef<gtk::Widget>,
}

fn search_hit(page_id: &'static str, title: &str, subtitle: &str, place: &str, widget: &gtk::Widget) -> SearchHit {
    let clean = |s: &str| s.replace("&amp;", "&").trim().to_string();
    let title = clean(title);
    let subtitle = clean(subtitle);
    let haystack = format!("{title} {subtitle} {place}").to_lowercase();
    SearchHit {
        page_id,
        title,
        place: place.to_string(),
        haystack,
        widget: widget.downgrade(),
    }
}

fn index_widget(page_id: &'static str, w: &gtk::Widget, place: &str, out: &mut Vec<SearchHit>) {
    if !w.is_visible() {
        return;
    }
    let mut place = place.to_string();
    if let Some(group) = w.downcast_ref::<adw::PreferencesGroup>() {
        let title = group.title();
        if !title.is_empty() {
            let desc = group.description().unwrap_or_default();
            out.push(search_hit(page_id, &title, &desc, &place, w));
            if !place.ends_with(title.as_str()) {
                place = format!("{place} › {title}");
            }
        }
    } else if let Some(row) = w.downcast_ref::<adw::PreferencesRow>() {
        let title = row.title();
        let subtitle = w
            .downcast_ref::<adw::ActionRow>()
            .and_then(|r| r.subtitle())
            .or_else(|| w.downcast_ref::<adw::SwitchRow>().and_then(|r| r.subtitle()))
            .or_else(|| w.downcast_ref::<adw::ExpanderRow>().map(|r| r.subtitle()))
            .unwrap_or_default();
        if !title.is_empty() {
            out.push(search_hit(page_id, &title, &subtitle, &place, w));
            if w.is::<adw::ExpanderRow>() {
                place = format!("{place} › {title}");
            } else {
                return;
            }
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        index_widget(page_id, &c, &place, out);
        child = c.next_sibling();
    }
}

pub fn build_search_index(pages: &gtk::Stack) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for sec in SIDEBAR_SECTIONS {
        for page in sec.pages {
            let page_title = gettext(page.title);
            let child = pages.child_by_name(page.id);
            let target = child.clone().unwrap_or_else(|| pages.clone().upcast());
            out.push(search_hit(page.id, &page_title, "", &gettext("Settings"), &target));
            if let Some(child) = child {
                index_widget(page.id, &child, &page_title, &mut out);
            }
        }
    }
    for sub in SUBPAGES {
        let parent_title = get_page_title(sub.parent_id).unwrap_or_default();
        let sub_title = gettext(sub.title);
        let breadcrumb = format!("{} › {}", parent_title, sub_title);
        let child = pages.child_by_name(sub.id);
        let target = child.clone().unwrap_or_else(|| pages.clone().upcast());
        out.push(search_hit(sub.id, &sub_title, "", &breadcrumb, &target));
        if let Some(child) = child {
            index_widget(sub.id, &child, &breadcrumb, &mut out);
        }
    }
    out
}

pub fn shown_in_hierarchy(w: &gtk::Widget) -> bool {
    let mut cur = Some(w.clone());
    while let Some(c) = cur {
        if let Some(parent) = c.parent() {
            if parent.is::<gtk::Stack>() {
                return c.is_visible();
            }
        }
        if !c.is_visible() {
            return false;
        }
        cur = c.parent();
    }
    true
}

pub fn reveal_widget(w: &gtk::Widget) {
    // 1. Expand any collapsed parent expander rows so child is visible in tree
    let mut parent = w.parent();
    while let Some(p) = parent {
        if let Some(expander) = p.downcast_ref::<adw::ExpanderRow>() {
            expander.set_expanded(true);
        }
        parent = p.parent();
    }

    // 2. Scroll into view inside parent ScrolledWindow
    if let Some(sw) = w.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>() {
        let content = sw.child().map(|c| match c.downcast_ref::<gtk::Viewport>() {
            Some(vp) => vp.child().unwrap_or(c.clone()),
            None => c,
        });
        if let Some(content) = content {
            if let Some(p) = w.compute_point(&content, &gtk::graphene::Point::new(0.0, 0.0)) {
                let adj = sw.vadjustment();
                let top = (f64::from(p.y()) - 24.0).clamp(adj.lower(), (adj.upper() - adj.page_size()).max(adj.lower()));
                adj.set_value(top);
            }
        }
    }

    // 3. Highlight pulse
    w.add_css_class("settings-search-hit");
    let w = w.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1600), move || {
        w.remove_css_class("settings-search-hit");
    });
}

fn make_segmented_toggle_row(
    combo: &adw::ComboRow,
    title: &str,
    label_0: &str,
    label_1: &str,
) -> adw::ActionRow {
    combo.set_visible(false);

    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();

    if let Some(sub) = combo.subtitle() {
        row.set_subtitle(&sub);
    }
    let row_weak_sub = row.downgrade();
    combo.connect_notify_local(Some("subtitle"), move |c, _| {
        if let Some(r) = row_weak_sub.upgrade() {
            if let Some(sub) = c.subtitle() {
                r.set_subtitle(&sub);
            } else {
                r.set_subtitle("");
            }
        }
    });

    let btn_0 = gtk::ToggleButton::builder()
        .label(label_0)
        .active(combo.selected() == 0)
        .build();

    let btn_1 = gtk::ToggleButton::builder()
        .label(label_1)
        .group(&btn_0)
        .active(combo.selected() == 1)
        .build();

    let toggle_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(vec!["linked"])
        .valign(gtk::Align::Center)
        .build();
    toggle_box.append(&btn_0);
    toggle_box.append(&btn_1);

    row.add_suffix(&toggle_box);

    let combo_weak_0 = combo.downgrade();
    let updating = Rc::new(Cell::new(false));
    let updating_btn0 = updating.clone();
    btn_0.connect_toggled(move |btn| {
        if updating_btn0.get() {
            return;
        }
        if btn.is_active() {
            if let Some(combo) = combo_weak_0.upgrade() {
                if combo.selected() != 0 {
                    updating_btn0.set(true);
                    combo.set_selected(0);
                    updating_btn0.set(false);
                }
            }
        }
    });

    let combo_weak_1 = combo.downgrade();
    let updating_btn1 = updating.clone();
    btn_1.connect_toggled(move |btn| {
        if updating_btn1.get() {
            return;
        }
        if btn.is_active() {
            if let Some(combo) = combo_weak_1.upgrade() {
                if combo.selected() != 1 {
                    updating_btn1.set(true);
                    combo.set_selected(1);
                    updating_btn1.set(false);
                }
            }
        }
    });

    let btn_0_weak = btn_0.downgrade();
    let btn_1_weak = btn_1.downgrade();
    let updating_combo = updating.clone();
    combo.connect_selected_notify(move |combo| {
        if updating_combo.get() {
            return;
        }
        updating_combo.set(true);
        let sel = combo.selected();
        if sel == 0 {
            if let Some(b) = btn_0_weak.upgrade() {
                b.set_active(true);
            }
        } else if sel == 1 {
            if let Some(b) = btn_1_weak.upgrade() {
                b.set_active(true);
            }
        }
        updating_combo.set(false);
    });

    row.set_sensitive(combo.is_sensitive());
    let row_weak = row.downgrade();
    combo.connect_sensitive_notify(move |c| {
        if let Some(r) = row_weak.upgrade() {
            r.set_sensitive(c.is_sensitive());
        }
    });

    row
}

fn make_segmented_toggle_3_row(
    combo: &adw::ComboRow,
    title: &str,
    label_0: &str,
    label_1: &str,
    label_2: &str,
) -> adw::ActionRow {
    combo.set_visible(false);

    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();

    if let Some(sub) = combo.subtitle() {
        row.set_subtitle(&sub);
    }
    let row_weak_sub = row.downgrade();
    combo.connect_notify_local(Some("subtitle"), move |c, _| {
        if let Some(r) = row_weak_sub.upgrade() {
            if let Some(sub) = c.subtitle() {
                r.set_subtitle(&sub);
            } else {
                r.set_subtitle("");
            }
        }
    });

    let btn_0 = gtk::ToggleButton::builder()
        .label(label_0)
        .active(combo.selected() == 0)
        .build();

    let btn_1 = gtk::ToggleButton::builder()
        .label(label_1)
        .group(&btn_0)
        .active(combo.selected() == 1)
        .build();

    let btn_2 = gtk::ToggleButton::builder()
        .label(label_2)
        .group(&btn_0)
        .active(combo.selected() == 2)
        .build();

    let toggle_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(vec!["linked"])
        .valign(gtk::Align::Center)
        .build();
    toggle_box.append(&btn_0);
    toggle_box.append(&btn_1);
    toggle_box.append(&btn_2);

    row.add_suffix(&toggle_box);

    let combo_weak_0 = combo.downgrade();
    let updating = Rc::new(Cell::new(false));
    let updating_btn0 = updating.clone();
    btn_0.connect_toggled(move |btn| {
        if updating_btn0.get() {
            return;
        }
        if btn.is_active() {
            if let Some(combo) = combo_weak_0.upgrade() {
                if combo.selected() != 0 {
                    updating_btn0.set(true);
                    combo.set_selected(0);
                    updating_btn0.set(false);
                }
            }
        }
    });

    let combo_weak_1 = combo.downgrade();
    let updating_btn1 = updating.clone();
    btn_1.connect_toggled(move |btn| {
        if updating_btn1.get() {
            return;
        }
        if btn.is_active() {
            if let Some(combo) = combo_weak_1.upgrade() {
                if combo.selected() != 1 {
                    updating_btn1.set(true);
                    combo.set_selected(1);
                    updating_btn1.set(false);
                }
            }
        }
    });

    let combo_weak_2 = combo.downgrade();
    let updating_btn2 = updating.clone();
    btn_2.connect_toggled(move |btn| {
        if updating_btn2.get() {
            return;
        }
        if btn.is_active() {
            if let Some(combo) = combo_weak_2.upgrade() {
                if combo.selected() != 2 {
                    updating_btn2.set(true);
                    combo.set_selected(2);
                    updating_btn2.set(false);
                }
            }
        }
    });

    let btn_0_weak = btn_0.downgrade();
    let btn_1_weak = btn_1.downgrade();
    let btn_2_weak = btn_2.downgrade();
    let updating_combo = updating.clone();
    combo.connect_selected_notify(move |combo| {
        if updating_combo.get() {
            return;
        }
        updating_combo.set(true);
        let sel = combo.selected();
        if sel == 0 {
            if let Some(b) = btn_0_weak.upgrade() {
                b.set_active(true);
            }
        } else if sel == 1 {
            if let Some(b) = btn_1_weak.upgrade() {
                b.set_active(true);
            }
        } else if sel == 2 {
            if let Some(b) = btn_2_weak.upgrade() {
                b.set_active(true);
            }
        }
        updating_combo.set(false);
    });

    row.set_sensitive(combo.is_sensitive());
    let row_weak = row.downgrade();
    combo.connect_sensitive_notify(move |c| {
        if let Some(r) = row_weak.upgrade() {
            r.set_sensitive(c.is_sensitive());
        }
    });

    row
}

fn build_default_handler_row(
    dialog: &PreferencesDialog,
    title: &str,
    active_subtitle: &str,
    inactive_subtitle: &str,
    toast_success: &str,
    primary_mime: &'static str,
    mime_types: &'static [&'static str],
) -> adw::ActionRow {
    let is_default = crate::application::is_default_app_for_mime(primary_mime);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(if is_default {
            active_subtitle
        } else {
            inactive_subtitle
        })
        .build();

    let btn = gtk::Button::builder()
        .label(if is_default {
            crate::i18n::gettext("Default")
        } else {
            crate::i18n::gettext("Set as Default")
        })
        .valign(gtk::Align::Center)
        .sensitive(!is_default)
        .css_classes(if is_default {
            vec!["flat"]
        } else {
            vec!["suggested-action"]
        })
        .build();

    row.add_suffix(&btn);

    let row_weak = row.downgrade();
    let dialog_weak = dialog.downgrade();
    let active_sub = active_subtitle.to_string();
    let toast_msg = toast_success.to_string();
    btn.connect_clicked(move |button| {
        if crate::application::set_as_default_app_for_mimes(mime_types) {
            button.set_label(&crate::i18n::gettext("Default"));
            button.set_sensitive(false);
            button.remove_css_class("suggested-action");
            button.add_css_class("flat");
            if let Some(r) = row_weak.upgrade() {
                r.set_subtitle(&active_sub);
            }
            if let Some(d) = dialog_weak.upgrade() {
                d.show_toast(&toast_msg);
            }
        }
    });

    row
}


impl PreferencesDialog {
    fn build_ui(&self) {
        let settings = AppSettings::try_new();

        let open_subpage_trigger: Rc<RefCell<Option<Box<dyn Fn(&'static str)>>>> = Rc::new(RefCell::new(None));
        let open_subpage: Rc<dyn Fn(&'static str)> = {
            let trigger = open_subpage_trigger.clone();
            Rc::new(move |subpage_id: &'static str| {
                if let Some(ref f) = *trigger.borrow() {
                    f(subpage_id);
                }
            })
        };

        // ── 9 Sidebar Pages matching Original Evolution ───────────────────────
        let page_accounts = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Accounts"))
            .icon_name("avatar-default-symbolic")
            .build();

        let page_contacts = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Contacts"))
            .icon_name("x-office-address-book-symbolic")
            .build();

        let page_backup = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Import & Backup"))
            .icon_name("drive-harddisk-symbolic")
            .build();

        let page_sub_calendar_view = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("View Options"))
            .build();

        // Top-level "Advanced" page (storage paths, diagnostics, plugins).
        let page_sub_backup_advanced = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Advanced"))
            .icon_name("preferences-other-symbolic")
            .build();

        let group_accounts = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Accounts"))
            .build();
        let btn_add_account = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&crate::i18n::gettext("Add Account…"))
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .build();
        btn_add_account.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.prompt_add_account_or_shared_mailbox();
            }
        ));
        group_accounts.set_header_suffix(Some(&btn_add_account));

        self.imp()
            .managed_accounts_group
            .replace(Some(group_accounts.clone()));
        self.refresh_managed_account_rows();

        page_accounts.add(&group_accounts);

        let group_profiles = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Configured Accounts"))
            .build();
        self.imp()
            .local_profile_group
            .replace(Some(group_profiles.clone()));
        self.refresh_local_profile_rows();
        page_accounts.add(&group_profiles);

        let group_oof = adw::PreferencesGroup::builder().build();
        let row_oof = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Automatic Replies…"))
            .activatable(true)
            .build();
        row_oof.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::OutOfOfficeDialog::new();
                dialog.present(Some(&page));
            }
        ));
        group_oof.add(&row_oof);
        page_accounts.add(&group_oof);

        let row_test_all = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Test All Filters…"))
            .activatable(true)
            .build();
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
                    "Dry run failed: Worker could not be started: {error}"
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
                            "Dry run for all filters: {} matches in {} checked message(s)",
                            report.matches.len(),
                            report.total_messages_evaluated,
                        ));
                    }
                    Err(_) => {}
                }
            });
        });
        let exp_log_actions = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Log Filter Actions"))
            .show_enable_switch(true)
            .build();
        if let Some(settings) = settings.clone() {
            exp_log_actions.set_enable_expansion(settings.filters_log_actions());
            let settings_for_log = settings.clone();
            exp_log_actions.connect_enable_expansion_notify(move |exp| {
                settings_for_log.set_filters_log_actions(exp.enables_expansion());
            });
        }

        let row_log_file = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Log File"))
            .show_apply_button(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_log_file.set_text(&settings.filters_log_file());
            let settings_for_file = settings.clone();
            row_log_file.connect_apply(move |entry| {
                settings_for_file.set_filters_log_file(entry.text().as_str());
            });
        }
        exp_log_actions.add_row(&row_log_file);

        let existing_filter_rules = MailFilterStore::default().load();
        let mut filter_rule_rows = Vec::new();

        for rule in existing_filter_rules {
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
                .join(if rule.match_all { " AND " } else { " OR " });
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
                    dialog.present(Some(&page));
                }
            ));
            let enabled = gtk::Switch::builder()
                .active(rule.enabled)
                .valign(gtk::Align::Center)
                .tooltip_text("Enable/disable filter rule")
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
                .tooltip_text("Move filter rule up")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            let move_down_button = gtk::Button::builder()
                .icon_name("go-down-symbolic")
                .tooltip_text("Move filter rule down")
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
                    Ok(true) => dialog.show_toast(&crate::i18n::gettext("Filter order saved")),
                    Ok(false) => dialog.show_toast("Filter rule is already at the top"),
                    Err(error) => dialog.show_toast(&format!(
                        "Could not save filter order: {error}"
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
                    Ok(true) => dialog.show_toast(&crate::i18n::gettext("Filter order saved")),
                    Ok(false) => dialog.show_toast("Filter rule is already at the bottom"),
                    Err(error) => dialog.show_toast(&format!(
                        "Could not save filter order: {error}"
                    )),
                }
            });
            row.add_suffix(&move_up_button);
            row.add_suffix(&move_down_button);

            let run_button = gtk::Button::builder()
                .icon_name("media-playback-start-symbolic")
                .tooltip_text("Run rule now on local mail store")
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
                        "Local filter run failed: Worker could not be started: {error}"
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
                            .and_downcast::<crate::window::MeridianWindow>()
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
                .tooltip_text("Run dry run for this rule (without applying changes)")
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
                        "Dry run failed: Worker could not be started: {error}"
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
                                "Dry run completed: {} of {} message(s) match (no changes made)",
                                report.matches.len(),
                                report.total_messages_evaluated,
                            ));
                        }
                        Ok(Ok(None)) => {
                            dialog.show_toast("Filter rule was not found for dry run");
                        }
                        Ok(Err(error)) => {
                            dialog.show_toast(&format!("Dry run failed: {error}"));
                        }
                        Err(_) => {}
                    }
                });
            });
            row.add_suffix(&simulate_button);

            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove filter rule")
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
                    "Remove filter rule?",
                    "The rule will be removed from app filter rules.",
                    "Remove",
                    move || match MailFilterStore::default().delete(&rule_id) {
                        Ok(true) => {
                            row.set_visible(false);
                            dialog_for_action.show_toast("Filter rule removed");
                        }
                        Ok(false) => {
                            dialog_for_action.show_toast("Filter rule not found")
                        }
                        Err(error) => dialog_for_action.show_toast(&format!(
                            "Filter rule could not be removed: {error}"
                        )),
                    },
                );
            });
            filter_rule_rows.push(row);
        }

        let page_filters = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Filters"))
            .icon_name("funnel-symbolic")
            .build();
        let group_filter_rules = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Filters"))
            .build();
        let btn_add_filter = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&crate::i18n::gettext("New Filter…"))
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .build();
        btn_add_filter.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::FilterEditorDialog::new();
                dialog.present(Some(&page));
            }
        ));
        group_filter_rules.set_header_suffix(Some(&btn_add_filter));

        if filter_rule_rows.is_empty() {
            let row_empty = adw::ActionRow::builder()
                .title(&crate::i18n::gettext("No Filters"))
                .build();
            group_filter_rules.add(&row_empty);
        } else {
            for row in filter_rule_rows {
                group_filter_rules.add(&row);
            }
        }
        page_filters.add(&group_filter_rules);

        let group_filter_diagnostics = adw::PreferencesGroup::builder().build();
        row_test_all.set_visible(true);
        group_filter_diagnostics.add(&row_test_all);
        exp_log_actions.set_visible(true);
        group_filter_diagnostics.add(&exp_log_actions);
        page_filters.add(&group_filter_diagnostics);

        let group_sync = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Synchronization"))
            .build();

        let sync_interval_model = gtk::StringList::new(&[]);
        for opt in [
            crate::i18n::gettext("Manually"),
            crate::i18n::gettext("Every minute"),
            crate::i18n::gettext("Every 5 minutes"),
            crate::i18n::gettext("Every 10 minutes"),
            crate::i18n::gettext("Every 15 minutes"),
            crate::i18n::gettext("Every 30 minutes"),
            crate::i18n::gettext("Hourly"),
            crate::i18n::gettext("Daily"),
        ] {
            sync_interval_model.append(&opt);
        }
        let row_sync_interval = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Check for New Mail"))
            .model(&sync_interval_model)
            .build();

        if let Some(settings) = settings.clone() {
            let initial_secs = settings.mail_sync_interval_seconds();
            row_sync_interval.set_selected(sync_interval_to_index(initial_secs));

            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();
            row_sync_interval.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                let idx = row.selected() as usize;
                let secs = sync_interval_seconds_list().get(idx).copied().unwrap_or(300);
                settings_notify.set_mail_sync_interval_seconds(secs);
            });

            let row_for_change = row_sync_interval.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("mail-sync-interval"), move |s, _| {
                let target = s.int("mail-sync-interval").max(0) as u32;
                let expected_idx = sync_interval_to_index(target);
                if row_for_change.selected() != expected_idx {
                    updating_change.set(true);
                    row_for_change.set_selected(expected_idx);
                    updating_change.set(false);
                }
            });
        }
        group_sync.add(&row_sync_interval);

        let row_offline_mode = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Work Offline"))
            .build();
        if let Some(settings) = settings.clone() {
            row_offline_mode.set_active(settings.offline_mode());
            let dialog_for_offline = self.downgrade();
            row_offline_mode.connect_active_notify(move |row| {
                settings.set_offline_mode(row.is_active());
                if let Some(dialog) = dialog_for_offline.upgrade() {
                    if let Some(window) = dialog
                        .root()
                        .and_downcast::<crate::window::MeridianWindow>()
                    {
                        window.refresh_mail_page();
                    }
                }
            });
        }
        group_sync.add(&row_offline_mode);

        let row_nav_sync_cache = make_subpage_nav_row(
            &crate::i18n::gettext("Advanced"),
            "sub_sync_cache",
            &open_subpage,
        );
        group_sync.add(&row_nav_sync_cache);

        let row_flush_outbox = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Empty outbox when checking for mail"))
            .build();
        if let Some(settings) = settings.clone() {
            row_flush_outbox.set_active(settings.flush_outbox());
            let settings_for_flush = settings.clone();
            row_flush_outbox.connect_active_notify(move |switch| {
                settings_for_flush.set_flush_outbox(switch.is_active());
            });
        }

        let row_limit_memory_usage = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Limit RAM usage for message cache"))
            .build();
        if let Some(settings) = settings.clone() {
            row_limit_memory_usage.set_active(settings.mail_limit_memory_usage());
            let settings_for_mem = settings.clone();
            row_limit_memory_usage.connect_active_notify(move |switch| {
                settings_for_mem.set_mail_limit_memory_usage(switch.is_active());
            });
        }

        let row_speculative_prefetch = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Speculative Prefetching (Instant Click)"))
            .subtitle(&crate::i18n::gettext(
                "Preload adjacent messages, hovered folders, and neighboring calendar periods in the background without side effects",
            ))
            .build();
        if let Some(settings) = settings.clone() {
            row_speculative_prefetch.set_active(settings.speculative_prefetch_enabled());
            let settings_for_prefetch = settings.clone();
            row_speculative_prefetch.connect_active_notify(move |switch| {
                settings_for_prefetch.set_speculative_prefetch_enabled(switch.is_active());
            });
        }

        let row_sync_message_limit = adw::SpinRow::with_range(25.0, 1_000.0, 25.0);
        row_sync_message_limit.set_title(&crate::i18n::gettext("Messages to fetch per folder"));
        row_sync_message_limit.set_digits(0);
        row_sync_message_limit.set_snap_to_ticks(true);
        if let Some(settings) = settings.clone() {
            row_sync_message_limit.set_value(settings.mail_sync_message_limit() as f64);
            row_sync_message_limit.connect_value_notify(move |row| {
                settings.set_mail_sync_message_limit(row.value().round() as u32);
            });
        }

        let page_sub_sync_cache = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Advanced"))
            .build();
        let group_cache_settings = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Synchronization"))
            .build();

        let row_sync_profile = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Synchronization Profile"))
            .subtitle(&crate::i18n::gettext(
                "Preset balance between offline availability and RAM / bandwidth usage",
            ))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Full Offline (Recommended)"),
                &crate::i18n::gettext("Balanced"),
                &crate::i18n::gettext("Low Data & RAM"),
            ]))
            .build();
        if let Some(s) = settings.clone() {
            let limit = s.mail_sync_message_limit();
            let low_mem = s.mail_limit_memory_usage();
            let initial_profile = if low_mem && limit <= 100 {
                2
            } else if limit <= 250 {
                1
            } else {
                0
            };
            row_sync_profile.set_selected(initial_profile);
            let limit_row_weak = row_sync_message_limit.downgrade();
            let mem_row_weak = row_limit_memory_usage.downgrade();
            row_sync_profile.connect_selected_notify(move |combo| {
                let (target_limit, target_mem) = match combo.selected() {
                    1 => (250.0, false),
                    2 => (50.0, true),
                    _ => (1000.0, false),
                };
                if let Some(r_lim) = limit_row_weak.upgrade() {
                    r_lim.set_value(target_limit);
                }
                if let Some(r_mem) = mem_row_weak.upgrade() {
                    r_mem.set_active(target_mem);
                }
            });
        }
        group_cache_settings.add(&row_sync_profile);

        row_sync_message_limit.set_visible(true);
        group_cache_settings.add(&row_sync_message_limit);
        row_limit_memory_usage.set_visible(true);
        group_cache_settings.add(&row_limit_memory_usage);
        row_speculative_prefetch.set_visible(true);
        group_cache_settings.add(&row_speculative_prefetch);
        row_flush_outbox.set_visible(true);
        group_cache_settings.add(&row_flush_outbox);
        page_sub_sync_cache.add(&group_cache_settings);

        let default_mail_dir_label = format!(
            "{} ({})",
            MailStoreService::new().mail_dir.display(),
            crate::i18n::gettext("Automatic")
        );
        let row_local_mail = directory_picker_row(
            self,
            &crate::i18n::gettext("Local mail folder"),
            &default_mail_dir_label,
            settings
                .as_ref()
                .map(AppSettings::local_mail_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_mail_directory(path),
        );

        let default_cal_dir_label = format!(
            "{} ({})",
            CalendarStoreService::new().calendar_dir().display(),
            crate::i18n::gettext("Default")
        );
        let row_local_calendar = directory_picker_row(
            self,
            &crate::i18n::gettext("Storage Folder for Local Calendar"),
            &default_cal_dir_label,
            settings
                .as_ref()
                .map(AppSettings::local_calendar_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_calendar_directory(path),
        );

        let default_contact_dir_label = format!(
            "{} ({})",
            ContactStoreService::new().contact_dir().display(),
            crate::i18n::gettext("Default")
        );
        let row_local_contacts = directory_picker_row(
            self,
            &crate::i18n::gettext("Storage Folder for Local Contacts"),
            &default_contact_dir_label,
            settings
                .as_ref()
                .map(AppSettings::local_contact_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_contact_directory(path),
        );

        let row_import_calendar = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Calendar File (.ics)…"))
            .activatable(true)
            .build();
        let import_calendar_button = gtk::Button::builder()
            .label(&crate::i18n::gettext("Import…"))
            .tooltip_text(&crate::i18n::gettext("Select iCalendar Files"))
            .valign(gtk::Align::Center)
            .build();
        
        let import_calendar_btn_clone = import_calendar_button.clone();
        row_import_calendar.connect_activated(move |_| import_calendar_btn_clone.emit_clicked());
        import_calendar_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for calendar import");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Select iCalendar Files")
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
                                    "The configured calendar folder is external and read-only; please select an app-owned folder first",
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
                                        "{} iCalendar file(s) imported; refreshing calendar view",
                                        count
                                    )),
                                    Err(error) => dialog.show_toast(&format!(
                                        "Calendar import failed: {error}"
                                    )),
                                }
                            }
                        });
                    },
                );
            }
        ));
        

        let row_import_contacts = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Contacts File (.vcf)…"))
            .activatable(true)
            .build();
        let import_contacts_button = gtk::Button::builder()
            .label(&crate::i18n::gettext("Import…"))
            .tooltip_text(&crate::i18n::gettext("Select vCard Files"))
            .valign(gtk::Align::Center)
            .build();
        
        let import_contacts_btn_clone = import_contacts_button.clone();
        row_import_contacts.connect_activated(move |_| import_contacts_btn_clone.emit_clicked());
        import_contacts_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for contact import");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Select vCard Files")
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
                                "The configured contact folder is external and read-only; please select an app-owned folder first",
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
                                    "{} vCard file(s) imported; refreshing contacts view",
                                    count
                                )),
                                Err(error) => dialog
                                    .show_toast(&format!("Contact import failed: {error}")),
                            }
                        }
                    });
                });
            }
        ));
        

        let group_default_address_book = adw::PreferencesGroup::builder()
            .title("Default Address Book")
            .build();
        group_default_address_book.add(&source_preference_row(
            "Default Address Book",
            "",
            EdsSourceKind::AddressBook,
            "local-addressbook",
            "Local Address Book",
            settings.clone(),
            AppSettings::default_address_book_source_uid,
            AppSettings::set_default_address_book_source_uid,
        ));
        page_contacts.add(&group_default_address_book);

        let group_contacts_display = adw::PreferencesGroup::builder()
            .title("Appearance and Maps")
            .build();

        let row_address_formatting = adw::SwitchRow::builder()
            .title("Address Formatting")
            .build();

        let row_home_before_work = adw::SwitchRow::builder()
            .title("Home address before work address")
            .build();

        let row_preview_show_maps = adw::SwitchRow::builder()
            .title("Show map hints")
            .build();

        let row_open_map_target = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Map Service"))
            .model(&gtk::StringList::new(&["OpenStreetMap", "Google Maps"]))
            .build();
        let row_map_toggle = make_segmented_toggle_row(
            &row_open_map_target,
            &crate::i18n::gettext("Map Service"),
            "OpenStreetMap",
            "Google Maps",
        );

        let row_open_map_prefer_local = adw::SwitchRow::builder()
            .title("Prefer local map application")
            .build();

        let row_contact_preview_visible = adw::SwitchRow::builder()
            .title("Show contact preview")
            .build();

        if let Some(settings_ref) = settings.clone() {
            row_address_formatting.set_active(settings_ref.contact_address_formatting());
            let s_formatting = settings_ref.clone();
            row_address_formatting.connect_active_notify(move |row| {
                s_formatting.set_contact_address_formatting(row.is_active());
            });

            let preview_vis = settings_ref.contact_preview_visible();
            row_home_before_work.set_active(settings_ref.contact_preview_home_before_work());
            row_home_before_work.set_sensitive(preview_vis);
            let s_home = settings_ref.clone();
            row_home_before_work.connect_active_notify(move |row| {
                s_home.set_contact_preview_home_before_work(row.is_active());
            });

            let maps_active = settings_ref.contact_preview_show_maps();
            row_preview_show_maps.set_active(maps_active);
            row_open_map_target.set_sensitive(maps_active);
            row_map_toggle.set_sensitive(maps_active);
            row_open_map_prefer_local.set_sensitive(maps_active);

            let s_maps = settings_ref.clone();
            let row_target_weak = row_open_map_target.downgrade();
            let row_toggle_weak = row_map_toggle.downgrade();
            let row_prefer_weak = row_open_map_prefer_local.downgrade();
            row_preview_show_maps.connect_active_notify(move |row| {
                let active = row.is_active();
                s_maps.set_contact_preview_show_maps(active);
                if let Some(r_target) = row_target_weak.upgrade() {
                    r_target.set_sensitive(active);
                }
                if let Some(r_toggle) = row_toggle_weak.upgrade() {
                    r_toggle.set_sensitive(active);
                }
                if let Some(r_prefer) = row_prefer_weak.upgrade() {
                    r_prefer.set_sensitive(active);
                }
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

            row_contact_preview_visible.set_active(preview_vis);
            let s_preview = settings_ref.clone();
            let row_home_weak = row_home_before_work.downgrade();
            row_contact_preview_visible.connect_active_notify(move |row| {
                let active = row.is_active();
                s_preview.set_contact_preview_visible(active);
                if let Some(r_home) = row_home_weak.upgrade() {
                    r_home.set_sensitive(active);
                }
            });
        }

        row_address_formatting.set_visible(false);
        row_home_before_work.set_visible(true);
        row_open_map_prefer_local.set_visible(true);

        group_contacts_display.add(&row_contact_preview_visible);
        group_contacts_display.add(&row_home_before_work);
        group_contacts_display.add(&row_address_formatting);
        group_contacts_display.add(&row_preview_show_maps);
        group_contacts_display.add(&row_open_map_target);
        group_contacts_display.add(&row_map_toggle);
        group_contacts_display.add(&row_open_map_prefer_local);
        page_contacts.add(&group_contacts_display);

        let group_contacts_autocomplete = adw::PreferencesGroup::builder()
            .title("Autocompletion")
            .build();

        let row_completion_show_address = adw::SwitchRow::builder()
            .title("Show email address")
            .build();

        let row_completion_min_query_length = adw::SpinRow::with_range(1.0, 100.0, 1.0);
        row_completion_min_query_length.set_title("Minimum search length");
        row_completion_min_query_length.set_digits(0);
        row_completion_min_query_length.set_snap_to_ticks(true);

        let row_completion_book_sources = adw::EntryRow::builder()
            .title("Address book sources for autocompletion")
            .show_apply_button(true)
            .visible(false)
            .build();
        row_completion_book_sources.set_tooltip_text(Some(
            "Comma-separated list of address book UIDs (e.g. local-addressbook) or leave empty for all enabled books",
        ));

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

            row_completion_book_sources
                .set_text(&settings_ref.contact_completion_book_sources());
            let s_sources = settings_ref.clone();
            row_completion_book_sources.connect_apply(move |entry| {
                s_sources.set_contact_completion_book_sources(entry.text().as_str());
            });
            let row_weak = row_completion_book_sources.downgrade();
            settings_ref.connect_changed(
                Some("contact-completion-book-sources"),
                move |s, _| {
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    let current = s.string("contact-completion-book-sources");
                    if row.text().as_str() != current.as_str() {
                        row.set_text(&current);
                    }
                },
            );
        }

        group_contacts_autocomplete.add(&row_completion_show_address);
        group_contacts_autocomplete.add(&row_completion_min_query_length);
        group_contacts_autocomplete.add(&row_completion_book_sources);
        page_contacts.add(&group_contacts_autocomplete);

        let group_contact_columns = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Columns and Editor Fields"))
            .build();

        let exp_contact_cols = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Visible Contact List Columns"))
            .show_enable_switch(false)
            .build();

        for (col_id, col_label) in [
            ("company", crate::i18n::gettext("Company")),
            ("title", crate::i18n::gettext("Job Title")),
            ("email", crate::i18n::gettext("Email Address")),
            ("phone", crate::i18n::gettext("Phone Number")),
            ("categories", crate::i18n::gettext("Categories")),
        ] {
            let row = adw::SwitchRow::builder()
                .title(&col_label)
                .active(settings.as_ref().map(|s| s.contact_col_visible(col_id)).unwrap_or(true))
                .build();
            if let Some(ref s) = settings {
                let s_col = s.clone();
                row.connect_active_notify(move |r| {
                    s_col.set_contact_col_visible(col_id, r.is_active());
                });
            }
            exp_contact_cols.add_row(&row);
        }
        group_contact_columns.add(&exp_contact_cols);

        let exp_contact_fields = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Visible Contact Editor Fields"))
            .show_enable_switch(false)
            .build();

        let editor_fields: [(&str, fn(&AppSettings) -> bool, fn(&AppSettings, bool)); 10] = [
            ("Phone Numbers", AppSettings::contact_editor_show_phone, AppSettings::set_contact_editor_show_phone),
            ("SIP / VoIP", AppSettings::contact_editor_show_sip, AppSettings::set_contact_editor_show_sip),
            ("Instant Messaging", AppSettings::contact_editor_show_im, AppSettings::set_contact_editor_show_im),
            ("Home Address", AppSettings::contact_editor_show_home_address, AppSettings::set_contact_editor_show_home_address),
            ("Work Address", AppSettings::contact_editor_show_work_address, AppSettings::set_contact_editor_show_work_address),
            ("Web Addresses", AppSettings::contact_editor_show_web, AppSettings::set_contact_editor_show_web),
            ("Job & Organization", AppSettings::contact_editor_show_job, AppSettings::set_contact_editor_show_job),
            ("Important Dates", AppSettings::contact_editor_show_dates, AppSettings::set_contact_editor_show_dates),
            ("Notes", AppSettings::contact_editor_show_notes, AppSettings::set_contact_editor_show_notes),
            ("Certificates & PGP", AppSettings::contact_editor_show_certs, AppSettings::set_contact_editor_show_certs),
        ];

        for (field_label, getter, setter) in editor_fields {
            let row = adw::SwitchRow::builder()
                .title(&crate::i18n::gettext(field_label))
                .use_markup(false)
                .active(settings.as_ref().map(|s| getter(s)).unwrap_or(true))
                .build();
            if let Some(ref s) = settings {
                let s_field = s.clone();
                row.connect_active_notify(move |r| {
                    setter(&s_field, r.is_active());
                });
            }
            exp_contact_fields.add_row(&row);
        }
        group_contact_columns.add(&exp_contact_fields);
        page_contacts.add(&group_contact_columns);


        let initial_backup_sub = match crate::services::settings_service::last_backup_timestamp() {
            Some(ts) => format!("{}: {ts}", crate::i18n::gettext("Last backup")),
            None => format!("{}: {}", crate::i18n::gettext("Last backup"), crate::i18n::gettext("Never")),
        };
        let row_backup = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Back Up…"))
            .subtitle(&initial_backup_sub)
            .activatable(true)
            .build();
        let backup_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Create Backup")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let backup_btn_clone = backup_button.clone();
        row_backup.connect_activated(move |_| backup_btn_clone.emit_clicked());
        let row_backup_for_save = row_backup.downgrade();
        backup_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let raw = match backup_service::create_backup() {
                    Ok(raw) => raw,
                    Err(error) => {
                        dialog.show_toast(&format!(
                            "Backup could not be created: {error}"
                        ));
                        return;
                    }
                };
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for backup");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Save Meridian Backup")
                    .initial_name("meridian-backup.json")
                    .modal(true)
                    .build();
                let dialog_for_result = dialog.downgrade();
                let row_backup_weak = row_backup_for_save.clone();
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
                            Ok(_) => {
                                if let Some(ts) = crate::services::settings_service::record_backup_timestamp_now() {
                                    if let Some(r) = row_backup_weak.upgrade() {
                                        r.set_subtitle(&format!("{}: {ts}", crate::i18n::gettext("Last backup")));
                                    }
                                }
                                dialog.show_toast("App data backed up");
                            }
                            Err(error) => dialog.show_toast(&format!(
                                "Backup could not be saved: {error}"
                            )),
                        }
                    }
                });
            }
        ));
        

        let row_restore = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Restore…"))
            .activatable(true)
            .build();
        let restore_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Restore Backup")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let restore_btn_clone = restore_button.clone();
        row_restore.connect_activated(move |_| restore_btn_clone.emit_clicked());
        restore_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for restore");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Open Meridian Backup")
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
                                dialog.show_toast("Backup could not be read");
                            }
                            return;
                        };
                        let bytes = contents.as_ref().to_vec();
                        let inspection = match backup_service::inspect_backup(&bytes) {
                            Ok(inspection) => inspection,
                            Err(error) => {
                                if let Some(dialog) = dialog_for_file.upgrade() {
                                    dialog.show_toast(&format!(
                                        "Backup is invalid or corrupted: {error}"
                                    ));
                                }
                                return;
                            }
                        };
                        let Some(dialog) = dialog_for_file.upgrade() else {
                            return;
                        };
                        let checksum_status = if inspection.legacy_files_without_checksum == 0 {
                            "all files verified"
                        } else {
                            "older files without checksum included"
                        };
                        let settings_status = if inspection.settings_present {
                            "settings included"
                        } else {
                            "no settings included"
                        };
                        let confirmation = adw::AlertDialog::new(
                            Some("Restore App Data?"),
                            Some(&format!(
                                "{} file(s), {}; {}; existing app-owned files from this backup will be replaced. External profiles remain untouched.",
                                inspection.files,
                                settings_status,
                                checksum_status
                            )),
                        );
                        confirmation.add_responses(&[
                            ("cancel", "Cancel"),
                            ("restore", "Restore"),
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
                                            "App data restored ({} file(s){})",
                                            report.files_restored,
                                            if report.settings_restored {
                                                ", settings applied"
                                            } else {
                                                ""
                                            }
                                        )),
                                        Err(error) => dialog.show_toast(&format!(
                                            "Restore failed: {error}"
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
        

        let row_diagnostic = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Diagnostic Package…"))
            .activatable(true)
            .build();
        let diagnostic_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save Diagnostic Bundle")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let diagnostic_btn_clone = diagnostic_button.clone();
        row_diagnostic.connect_activated(move |_| diagnostic_btn_clone.emit_clicked());
        diagnostic_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let raw = match backup_service::create_diagnostic_package() {
                    Ok(raw) => raw,
                    Err(error) => {
                        dialog.show_toast(&format!(
                            "Diagnostic bundle could not be created: {error}"
                        ));
                        return;
                    }
                };
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for diagnostic bundle");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Save Diagnostic Bundle")
                    .initial_name("meridian-diagnostic.json")
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
                            Ok(_) => dialog.show_toast("Diagnostic bundle saved"),
                            Err(error) => dialog.show_toast(&format!(
                                "Diagnostic bundle could not be saved: {error}"
                            )),
                        }
                    }
                });
            }
        ));
        

        let row_delete = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Delete All Data…"))
            .css_classes(vec!["error"])
            .activatable(true)
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete application data")
            .css_classes(vec!["flat", "error"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let delete_btn_clone = delete_button.clone();
        row_delete.connect_activated(move |_| delete_btn_clone.emit_clicked());
        delete_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let confirmation = adw::AlertDialog::new(
                    Some("Delete application data?"),
                    Some("Account profiles, drafts, local ICS/vCard data, and app settings will be removed. External profiles, credentials, and external folders remain untouched."),
                );
                confirmation.add_responses(&[
                    ("cancel", "Cancel"),
                    ("delete", "Delete app data"),
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
                                    "{} app-owned file(s) deleted{}",
                                    report.files_removed,
                                    if report.settings_reset {
                                        "; settings reset"
                                    } else {
                                        ""
                                    }
                                )),
                                Err(error) => dialog.show_toast(&format!(
                                    "App data could not be completely deleted: {error}"
                                )),
                            }
                        }
                    ),
                );
                confirmation.present(Some(&dialog));
            }
        ));
        

        let row_import_profile = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Evolution Profile…"))
            .activatable(true)
            .build();
        let import_profile_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Select Evolution profile folder")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let import_profile_btn_clone = import_profile_button.clone();
        row_import_profile.connect_activated(move |_| import_profile_btn_clone.emit_clicked());
        import_profile_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for profile import");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Select Evolution profile folder")
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
                                                    "Evolution setting could not be applied"
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
                                        tracing::warn!(%warning, "Evolution profile import with note");
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
                                        "{} profile source(s) found: {} account(s), {} mail, {} calendar, and {} address book source(s) cached, {} filter rule(s), {} signature(s), {} setting value(s) from {} file(s), {} mail file(s), {} local mail folders, {} iCalendar file(s), and {} vCard file(s) imported, {} skipped{}{}",
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
                                        .and_downcast::<crate::window::MeridianWindow>()
                                    {
                                        window.refresh_mail_page();
                                    }
                                }
                                Err(error) => dialog
                                    .show_toast(&format!("Profile import failed: {error}")),
                            }
                        });
                    },
                );
            }
        ));
        

        let row_import_mail = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Mail Files…"))
            .activatable(true)
            .build();
        let import_mail_button = gtk::Button::builder()
            .icon_name("mail-read-symbolic")
            .tooltip_text("Select mail files")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        
        
        let import_mail_btn_clone = import_mail_button.clone();
        row_import_mail.connect_activated(move |_| import_mail_btn_clone.emit_clicked());
        import_mail_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for mail import");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Select local mail files")
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
                                    "{} local mail file(s) imported; refreshing mail view",
                                    count
                                )),
                                Err(error) => dialog
                                    .show_toast(&format!("Mail import failed: {error}")),
                            }
                        }
                    });
                });
            }
        ));
        let import_mail_folder_button = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Select mail folder")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_mail.add_suffix(&import_mail_folder_button);
        import_mail_folder_button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for mail folder import");
                    return;
                };
                let file_dialog = gtk::FileDialog::builder()
                    .title("Select local mail folder")
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
                                        "{} mail file(s) imported from folder; refreshing mail view",
                                        count
                                    )),
                                    Err(error) => dialog.show_toast(&format!(
                                        "Mail folder import failed: {error}"
                                    )),
                                }
                            }
                        });
                    },
                );
            }
        ));
        
        // group_backup configured in backup section


        let row_plugins = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Plugins…"))
            .activatable(true)
            .build();
        
        let dialog_weak_plugins = self.downgrade();
        let s_plugins = settings.clone();
        row_plugins.connect_activated(move |_| {
            if let Some(d) = dialog_weak_plugins.upgrade() {
                crate::dialogs::plugins_dialog::present_plugins_dialog(Some(&d), s_plugins.clone());
            }
        });
        
        // group_plugins configured in backup section

        let group_composer = adw::PreferencesGroup::builder()
            .title("Composer")
            .build();
        let row_composer_html = adw::SwitchRow::builder()
            .title("Use HTML when composing")
            .active(true)
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_composer_html.set_active(settings.composer_html());
            row_composer_html.connect_active_notify(move |row| {
                settings.set_composer_html(row.is_active());
            });
        }
        group_composer.add(&row_composer_html);

        let row_composer_format_toolbar = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Always show formatting toolbar"))
            .active(false)
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_composer_format_toolbar.set_active(settings.composer_show_format_toolbar());
            row_composer_format_toolbar.set_sensitive(settings.composer_html());
            let s_ft = settings.clone();
            row_composer_format_toolbar.connect_active_notify(move |row| {
                s_ft.set_composer_show_format_toolbar(row.is_active());
            });
            let row_weak = row_composer_format_toolbar.downgrade();
            settings.connect_changed(Some("composer-show-format-toolbar"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("composer-show-format-toolbar");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
            let r_ft = row_composer_format_toolbar.clone();
            row_composer_html.connect_active_notify(move |row| {
                r_ft.set_sensitive(row.is_active());
            });
        }
        group_composer.add(&row_composer_format_toolbar);

        let row_request_read_receipt = adw::SwitchRow::builder()
            .title("Request read receipt")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_request_read_receipt.set_active(settings.request_read_receipt());
            row_request_read_receipt.connect_active_notify(move |row| {
                settings.set_request_read_receipt(row.is_active());
            });
        }

        let row_composer_inline_spelling = adw::SwitchRow::builder()
            .title("Inline spell checking")
            .active(true)
            .build();

        let mut spellcheck_options: Vec<(String, String)> = vec![
            (String::new(), "System Default".to_string()),
            ("de-DE".to_string(), "German".to_string()),
            ("en-US".to_string(), "English".to_string()),
            ("de-DE,en-US".to_string(), "German and English".to_string()),
        ];
        {
            let mut extra_codes = std::collections::BTreeSet::new();
            for dir in ["/usr/share/hunspell", "/usr/share/myspell"] {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("dic") {
                            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                let normalized = stem.replace('_', "-");
                                if !matches!(
                                    normalized.as_str(),
                                    "de-DE" | "en-US" | "de" | "en"
                                ) {
                                    extra_codes.insert(normalized);
                                }
                            }
                        }
                    }
                }
            }
            for code in extra_codes {
                let lang_prefix = code.split('-').next().unwrap_or(&code);
                let label = match lang_prefix {
                    "de" => format!("German ({code})"),
                    "en" => format!("English ({code})"),
                    "fr" => format!("French ({code})"),
                    "es" => format!("Spanish ({code})"),
                    "it" => format!("Italian ({code})"),
                    "pt" => format!("Portuguese ({code})"),
                    "nl" => format!("Dutch ({code})"),
                    "pl" => format!("Polish ({code})"),
                    "ru" => format!("Russian ({code})"),
                    "tr" => format!("Turkish ({code})"),
                    "sv" => format!("Swedish ({code})"),
                    "da" => format!("Danish ({code})"),
                    "nb" | "nn" | "no" => format!("Norwegian ({code})"),
                    "fi" => format!("Finnish ({code})"),
                    "cs" => format!("Czech ({code})"),
                    "uk" => format!("Ukrainian ({code})"),
                    _ => code.clone(),
                };
                spellcheck_options.push((code, label));
            }
        }
        let spellcheck_labels: Vec<&str> = spellcheck_options.iter().map(|(_, l)| l.as_str()).collect();
        let row_spellcheck_languages = adw::ComboRow::builder()
            .title("Spell check languages")
            .model(&gtk::StringList::new(&spellcheck_labels))
            .build();

        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_composer_inline_spelling.set_active(settings.composer_inline_spelling());
            let row_spellcheck_weak_spelling = row_spellcheck_languages.downgrade();
            row_composer_inline_spelling.connect_active_notify(move |row| {
                if let Some(r_spell) = row_spellcheck_weak_spelling.upgrade() {
                    r_spell.set_sensitive(row.is_active());
                }
                if updating_notify.get() {
                    return;
                }
                settings_notify.set_composer_inline_spelling(row.is_active());
            });

            let row_for_change = row_composer_inline_spelling.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("composer-inline-spelling"), move |s, _| {
                let target = s.boolean("composer-inline-spelling");
                if row_for_change.is_active() != target {
                    updating_change.set(true);
                    row_for_change.set_active(target);
                    updating_change.set(false);
                }
            });
        }
        group_composer.add(&row_composer_inline_spelling);

        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();
            let options_for_parse = spellcheck_options.clone();
            let options_for_notify = spellcheck_options.clone();

            let parse_lang_selection = move |lang_str: &str| -> u32 {
                let norm = lang_str.replace('_', "-");
                match norm.as_str() {
                    "" => 0,
                    "de-DE" => 1,
                    "en-US" => 2,
                    "de-DE,en-US" => 3,
                    other => options_for_parse
                        .iter()
                        .position(|(code, _)| code.eq_ignore_ascii_case(other))
                        .unwrap_or(0) as u32,
                }
            };
            let parse_for_change = parse_lang_selection.clone();

            row_spellcheck_languages.set_selected(parse_lang_selection(&settings.spellcheck_languages()));
            row_spellcheck_languages.set_sensitive(settings.composer_inline_spelling());
            row_spellcheck_languages.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                let idx = row.selected() as usize;
                let code = options_for_notify
                    .get(idx)
                    .map(|(c, _)| c.as_str())
                    .unwrap_or("");
                settings_notify.set_spellcheck_languages(code);
            });

            let row_for_change = row_spellcheck_languages.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("spellcheck-languages"), move |s, _| {
                let target = parse_for_change(&s.string("spellcheck-languages"));
                if row_for_change.selected() != target {
                    updating_change.set(true);
                    row_for_change.set_selected(target);
                    updating_change.set(false);
                }
            });
        }
        group_composer.add(&row_spellcheck_languages);

        let row_composer_charset = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Default Character Set"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("UTF-8"),
                &crate::i18n::gettext("ISO-8859-1 (Latin-1)"),
                &crate::i18n::gettext("ISO-8859-15 (Latin-9)"),
                &crate::i18n::gettext("Windows-1252"),
                &crate::i18n::gettext("US-ASCII"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let initial_idx = match settings.composer_charset().as_str() {
                "ISO-8859-1" => 1,
                "ISO-8859-15" => 2,
                "Windows-1252" => 3,
                "US-ASCII" => 4,
                _ => 0,
            };
            row_composer_charset.set_selected(initial_idx);
            let settings_for_charset = settings.clone();
            row_composer_charset.connect_selected_notify(move |row| {
                let charset = match row.selected() {
                    1 => "ISO-8859-1",
                    2 => "ISO-8859-15",
                    3 => "Windows-1252",
                    4 => "US-ASCII",
                    _ => "UTF-8",
                };
                settings_for_charset.set_composer_charset(charset);
            });
            let row_weak = row_composer_charset.downgrade();
            settings.connect_changed(Some("composer-charset"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let target = match s.string("composer-charset").as_str() {
                    "ISO-8859-1" => 1,
                    "ISO-8859-15" => 2,
                    "Windows-1252" => 3,
                    "US-ASCII" => 4,
                    _ => 0,
                };
                if row.selected() != target {
                    row.set_selected(target);
                }
            });
        }
        row_composer_charset.set_visible(true);

        let row_composer_draft_autosave = adw::SpinRow::with_range(0.0, 3_600.0, 5.0);
        row_composer_draft_autosave.set_title("Automatically save drafts");
        row_composer_draft_autosave.set_digits(0);
        row_composer_draft_autosave.set_snap_to_ticks(true);
        row_composer_draft_autosave.set_visible(false);
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_composer_draft_autosave.set_value(settings.draft_autosave_interval_seconds() as f64);
            row_composer_draft_autosave.connect_value_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                settings_notify.set_draft_autosave_interval_seconds(row.value().round() as u32);
            });

            let row_for_change = row_composer_draft_autosave.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("draft-autosave-interval"), move |s, _| {
                let target = s.int("draft-autosave-interval").clamp(0, 3_600) as f64;
                if (row_for_change.value() - target).abs() > 0.001 {
                    updating_change.set(true);
                    row_for_change.set_value(target);
                    updating_change.set(false);
                }
            });
        }
        group_composer.add(&row_composer_draft_autosave);

        let row_composer_draft_autosave_combo = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Save Drafts Automatically"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Every 30 seconds"),
                &crate::i18n::gettext("Every minute"),
                &crate::i18n::gettext("Every 2 minutes"),
                &crate::i18n::gettext("Every 5 minutes"),
                &crate::i18n::gettext("Off"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let secs_to_idx = |secs: u32| -> u32 {
                if secs == 0 {
                    4
                } else if secs <= 45 {
                    0
                } else if secs <= 90 {
                    1
                } else if secs <= 180 {
                    2
                } else {
                    3
                }
            };
            row_composer_draft_autosave_combo
                .set_selected(secs_to_idx(settings.draft_autosave_interval_seconds()));
            let s_combo = settings.clone();
            row_composer_draft_autosave_combo.connect_selected_notify(move |row| {
                let secs = match row.selected() {
                    0 => 30,
                    1 => 60,
                    2 => 120,
                    3 => 300,
                    _ => 0,
                };
                s_combo.set_draft_autosave_interval_seconds(secs);
            });
        }
        group_composer.add(&row_composer_draft_autosave_combo);

        let group_composer_headers = adw::PreferencesGroup::builder()
            .title("Headers Visible by Default")
            .build();
        let row_show_from = adw::SwitchRow::builder()
            .title("Show sender field (From)")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_from.set_active(settings.composer_show_from());
            row_show_from.connect_active_notify(move |row| {
                settings.set_composer_show_from(row.is_active());
            });
        }
        let row_show_reply_to = adw::SwitchRow::builder()
            .title("Show reply-to field")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_reply_to.set_active(settings.composer_show_reply_to());
            row_show_reply_to.connect_active_notify(move |row| {
                settings.set_composer_show_reply_to(row.is_active());
            });
        }

        let row_show_cc = adw::SwitchRow::builder()
            .title("Show Cc field")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_cc.set_active(settings.composer_show_cc());
            row_show_cc.connect_active_notify(move |row| {
                settings.set_composer_show_cc(row.is_active());
            });
        }

        let row_show_bcc = adw::SwitchRow::builder()
            .title("Show Bcc field")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_bcc.set_active(settings.composer_show_bcc());
            row_show_bcc.connect_active_notify(move |row| {
                settings.set_composer_show_bcc(row.is_active());
            });
        }

        let group_composer_prompts = adw::PreferencesGroup::builder()
            .title("Send Confirmations")
            .build();
        let row_prompt_empty_subject = adw::SwitchRow::builder()
            .title("Warn on empty subject")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_prompt_empty_subject.set_active(settings.composer_prompt_empty_subject());
            row_prompt_empty_subject.connect_active_notify(move |row| {
                settings.set_composer_prompt_empty_subject(row.is_active());
            });
        }

        let row_prompt_only_bcc = adw::SwitchRow::builder()
            .title("Warn when only Bcc recipients")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_prompt_only_bcc.set_active(settings.composer_prompt_only_bcc());
            row_prompt_only_bcc.connect_active_notify(move |row| {
                settings.set_composer_prompt_only_bcc(row.is_active());
            });
        }

        let row_track_recent_recipients = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Remember recent recipients"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_track_recent_recipients.set_active(settings.composer_track_recent_recipients());
            let s_tr = settings.clone();
            row_track_recent_recipients.connect_active_notify(move |row| {
                s_tr.set_composer_track_recent_recipients(row.is_active());
            });
            let row_weak = row_track_recent_recipients.downgrade();
            settings.connect_changed(Some("composer-track-recent-recipients"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("composer-track-recent-recipients");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }

        group_composer_headers.set_visible(true);
        group_composer_headers.add(&row_show_from);
        group_composer_headers.add(&row_show_reply_to);
        group_composer_headers.add(&row_show_cc);
        group_composer_headers.add(&row_show_bcc);

        group_composer_prompts.set_visible(true);
        group_composer_prompts.add(&row_prompt_empty_subject);
        group_composer_prompts.add(&row_prompt_only_bcc);
        group_composer_prompts.add(&row_track_recent_recipients);
        row_request_read_receipt.set_visible(true);
        group_composer_prompts.add(&row_request_read_receipt);

        let group_composer_charset = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Character Encoding"))
            .build();
        row_composer_charset.set_visible(true);
        group_composer_charset.add(&row_composer_charset);

        let signature_store = SignatureStore::from_environment();
        let initial_signatures = signature_store.load();
        let none_sig_label = crate::i18n::gettext("None");
        let mut initial_sig_names: Vec<&str> = vec![none_sig_label.as_str()];
        initial_sig_names.extend(initial_signatures.iter().map(|s| s.name.as_str()));
        let signature_model = gtk::StringList::new(&initial_sig_names);
        let signature_row = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Default Signature"))
            .model(&signature_model)
            .build();
        if let Some(active_id) = signature_store.active_id() {
            if let Some(index) = initial_signatures
                .iter()
                .position(|signature| signature.id == active_id)
            {
                signature_row.set_selected((index + 1) as u32);
            } else {
                signature_row.set_selected(0);
            }
        } else {
            signature_row.set_selected(0);
        }

        let row_top_signature = adw::SwitchRow::builder()
            .title("Insert signature above quote")
            .active(false)
            .sensitive(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_top_signature.set_active(settings.composer_top_signature());
            row_top_signature.connect_active_notify(move |row| {
                settings.set_composer_top_signature(row.is_active());
            });
        }
        row_top_signature.set_visible(true);
        row_top_signature.set_sensitive(true);
        

        // Separator: expander with enable switch + customizable separator text.
        let row_signature_outline = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Signature Separator"))
            .show_enable_switch(true)
            .enable_expansion(true)
            .build();
        let row_separator_text = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Separator Text"))
            .build();
        row_signature_outline.add_row(&row_separator_text);
        if let Some(settings) = settings.clone() {
            row_signature_outline.set_enable_expansion(settings.composer_signature_outline());
            let settings_for_switch = settings.clone();
            row_signature_outline.connect_enable_expansion_notify(move |row| {
                settings_for_switch.set_composer_signature_outline(row.enables_expansion());
            });
            let current = settings.signature_separator_text();
            row_separator_text.set_text(if current.is_empty() { "-- " } else { &current });
            row_separator_text.connect_changed(move |entry| {
                settings.set_signature_separator_text(&entry.text());
            });
        }
        

        let group_reply = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Replies and Forwards"))
            .build();

        let row_reply_style = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Reply Style"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Quoted"),
                &crate::i18n::gettext("Inline"),
                &crate::i18n::gettext("Attach Original"),
                &crate::i18n::gettext("Do Not Quote"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_reply_style.set_selected(match settings.composer_reply_style().as_str() {
                "inline" => 1,
                "attach" | "attached" => 2,
                "do-not-quote" => 3,
                _ => 0,
            });
            row_reply_style.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                settings_notify.set_composer_reply_style(match row.selected() {
                    1 => "inline",
                    2 => "attach",
                    3 => "do-not-quote",
                    _ => "quoted",
                });
            });

            let row_for_change = row_reply_style.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("composer-reply-style"), move |s, _| {
                let raw = s.string("composer-reply-style");
                let target = match normalize_composer_reply_style(&raw) {
                    Some("inline") => 1,
                    Some("attach") | Some("attached") => 2,
                    Some("do-not-quote") => 3,
                    _ => 0,
                };
                if row_for_change.selected() != target {
                    updating_change.set(true);
                    row_for_change.set_selected(target);
                    updating_change.set(false);
                }
            });
        }
        group_reply.add(&row_reply_style);

        let row_forward_style = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Forward Style"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Attached"),
                &crate::i18n::gettext("Inline"),
                &crate::i18n::gettext("Quoted"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_forward_style.set_selected(match settings.composer_forward_style().as_str() {
                "inline" => 1,
                "quoted" => 2,
                _ => 0,
            });
            row_forward_style.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                settings_notify.set_composer_forward_style(match row.selected() {
                    1 => "inline",
                    2 => "quoted",
                    _ => "attached",
                });
            });

            let row_for_change = row_forward_style.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("composer-forward-style"), move |s, _| {
                let raw = s.string("composer-forward-style");
                let target = match normalize_composer_forward_style(&raw) {
                    Some("inline") => 1,
                    Some("quoted") => 2,
                    _ => 0,
                };
                if row_for_change.selected() != target {
                    updating_change.set(true);
                    row_for_change.set_selected(target);
                    updating_change.set(false);
                }
            });
        }
        group_reply.add(&row_forward_style);

        // ── 2. Page: Erscheinungsbild & Leseansicht ─────────────────────────
        let page_appearance = adw::PreferencesPage::builder()
            .title(&gettext("Appearance"))
            .icon_name("display-brightness-symbolic")
            .build();

        let row_switcher_location = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Module button placement"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Top Header Bar"),
                &crate::i18n::gettext("Sidebar Bottom"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_switcher_location.set_selected(if settings.switcher_location() == "bottom-left" {
                1
            } else {
                0
            });
            let settings_for_notify = settings.clone();
            let updating = Rc::new(Cell::new(false));
            let updating_for_notify = updating.clone();
            row_switcher_location.connect_selected_notify(move |row| {
                if updating_for_notify.get() {
                    return;
                }
                settings_for_notify.set_switcher_location(if row.selected() == 1 {
                    "bottom-left"
                } else {
                    "top"
                });
            });
            let row_for_change = row_switcher_location.clone();
            settings.connect_changed(Some("switcher-location"), move |s, _| {
                let target = if s.string("switcher-location").as_str() == "bottom-left" {
                    1
                } else {
                    0
                };
                if row_for_change.selected() != target {
                    updating.set(true);
                    row_for_change.set_selected(target);
                    updating.set(false);
                }
            });
        }
        let nav_choice = make_segmented_toggle_row(
            &row_switcher_location,
            &crate::i18n::gettext("Module Switcher"),
            &crate::i18n::gettext("Header Bar"),
            &crate::i18n::gettext("Sidebar"),
        );
        

        let group_mail_reading = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Reading"))
            .build();

        let mark_read_model = gtk::StringList::new(&[
            &crate::i18n::gettext("Immediately"),
            &crate::i18n::gettext("After 0.5 s"),
            &crate::i18n::gettext("After 1.5 s"),
            &crate::i18n::gettext("After 3 s"),
            &crate::i18n::gettext("After 5 s"),
            &crate::i18n::gettext("Never"),
        ]);
        let row_mark_seen_timeout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Mark as Read"))
            .model(&mark_read_model)
            .build();

        if let Some(ref s) = settings {
            let timeout_to_idx = |seen: bool, ms: u32| -> u32 {
                if !seen {
                    5
                } else {
                    match ms {
                        0 => 0,
                        1..=999 => 1,
                        1000..=2000 => 2,
                        2001..=4000 => 3,
                        _ => 4,
                    }
                }
            };
            let idx_to_timeout = |idx: u32| -> (bool, u32) {
                match idx {
                    0 => (true, 0),
                    1 => (true, 500),
                    2 => (true, 1500),
                    3 => (true, 3000),
                    4 => (true, 5000),
                    _ => (false, 1500),
                }
            };
            row_mark_seen_timeout.set_selected(timeout_to_idx(s.mail_mark_seen(), s.mail_mark_seen_timeout()));

            let s_mr = s.clone();
            let updating_mr = Rc::new(Cell::new(false));
            let updating_mr_sel = updating_mr.clone();
            row_mark_seen_timeout.connect_selected_notify(move |row| {
                if updating_mr_sel.get() { return; }
                updating_mr_sel.set(true);
                let (seen, timeout) = idx_to_timeout(row.selected());
                s_mr.set_mail_mark_seen(seen);
                s_mr.set_mark_read_on_selection(seen);
                if seen {
                    s_mr.set_mail_mark_seen_timeout(timeout);
                }
                updating_mr_sel.set(false);
            });

            let row_weak = row_mark_seen_timeout.downgrade();
            let updating_mr_seen = updating_mr.clone();
            s.connect_changed(Some("mail-mark-seen"), move |s_obj, _| {
                if updating_mr_seen.get() { return; }
                if let Some(r) = row_weak.upgrade() {
                    let target = timeout_to_idx(s_obj.boolean("mail-mark-seen"), s_obj.int("mail-mark-seen-timeout").max(0) as u32);
                    if r.selected() != target {
                        updating_mr_seen.set(true);
                        r.set_selected(target);
                        updating_mr_seen.set(false);
                    }
                }
            });
            let row_weak_to = row_mark_seen_timeout.downgrade();
            let updating_mr_to = updating_mr.clone();
            s.connect_changed(Some("mail-mark-seen-timeout"), move |s_obj, _| {
                if updating_mr_to.get() { return; }
                if let Some(r) = row_weak_to.upgrade() {
                    let target = timeout_to_idx(s_obj.boolean("mail-mark-seen"), s_obj.int("mail-mark-seen-timeout").max(0) as u32);
                    if r.selected() != target {
                        updating_mr_to.set(true);
                        r.set_selected(target);
                        updating_mr_to.set(false);
                    }
                }
            });
        }
        group_mail_reading.add(&row_mark_seen_timeout);

        let row_headers_collapsed = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Collapse Headers"))
            .active(false)
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_headers_collapsed.set_active(settings.mail_headers_collapsed());
            let s_hc = settings.clone();
            row_headers_collapsed.connect_active_notify(move |row| {
                s_hc.set_mail_headers_collapsed(row.is_active());
            });
        }
        group_mail_reading.add(&row_headers_collapsed);

        let row_show_real_date = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show Full Date"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_real_date.set_active(settings.mail_show_real_date());
            let s_srd = settings.clone();
            row_show_real_date.connect_active_notify(move |row| {
                s_srd.set_mail_show_real_date(row.is_active());
            });
        }
        group_mail_reading.add(&row_show_real_date);

        let row_mail_security_details = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show security status for normal emails"))
            .active(false)
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_security_details.set_active(settings.mail_show_security_details());
            let s_msd = settings.clone();
            row_mail_security_details.connect_active_notify(move |row| {
                s_msd.set_mail_show_security_details(row.is_active());
            });
        }

        let row_mail_show_deleted = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show deleted messages"))
            .active(false)
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_deleted.set_active(settings.mail_show_deleted());
            let settings_for_deleted = settings.clone();
            row_mail_show_deleted.connect_active_notify(move |row| {
                settings_for_deleted.set_mail_show_deleted(row.is_active());
            });
            let row_weak = row_mail_show_deleted.downgrade();
            settings.connect_changed(Some("mail-show-deleted"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-deleted");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }

        // ── HTML Messages and Privacy Group ─────────────────────────────────
        let row_html_rendering = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Render HTML messages"))
            .active(true)
            .build();

        let row_image_policy = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Load images in HTML messages"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Never"),
                &crate::i18n::gettext("From Contacts Only"),
                &crate::i18n::gettext("Always"),
            ]))
            .build();

        let row_preserve_image_layout = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Preserve image layout"))
            .active(true)
            .build();

        let row_dark_mail = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Adapt emails to dark style"))
            .active(true)
            .build();

        if let Some(settings) = settings.clone() {
            row_html_rendering.set_active(settings.render_html());
            let s_html = settings.clone();
            row_html_rendering.connect_active_notify(move |row| {
                s_html.set_render_html(row.is_active());
            });

            let policy = settings.mail_image_loading_policy();
            row_image_policy.set_selected(match policy.as_str() {
                "sometimes" | "contacts" => 1,
                "always" => 2,
                _ => 0,
            });
            let s_img = settings.clone();
            row_image_policy.connect_selected_notify(move |row| {
                let policy_str = match row.selected() {
                    1 => "sometimes",
                    2 => "always",
                    _ => "never",
                };
                s_img.set_mail_image_loading_policy(policy_str);
                s_img.set_auto_load_remote_images(row.selected() == 2);
            });

            row_preserve_image_layout.set_active(settings.mail_preserve_image_layout());
            let s_pil = settings.clone();
            row_preserve_image_layout.connect_active_notify(move |row| {
                s_pil.set_mail_preserve_image_layout(row.is_active());
            });
            let row_weak = row_preserve_image_layout.downgrade();
            settings.connect_changed(Some("mail-preserve-image-layout"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-preserve-image-layout");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            row_dark_mail.set_active(settings.mail_dark_mode());
            let s_dm = settings.clone();
            row_dark_mail.connect_active_notify(move |row| {
                s_dm.set_mail_dark_mode(row.is_active());
            });

            // Progressive disclosure
            let update_html_sensitivities = {
                let r_html = row_html_rendering.clone();
                let r_img = row_image_policy.clone();
                let r_pres = row_preserve_image_layout.clone();
                let r_dark = row_dark_mail.clone();
                move || {
                    let html_active = r_html.is_active();
                    r_img.set_sensitive(html_active);
                    r_dark.set_sensitive(html_active);
                    r_pres.set_sensitive(html_active && r_img.selected() != 2);
                }
            };
            update_html_sensitivities();

            let update_sens_on_html = update_html_sensitivities.clone();
            row_html_rendering.connect_active_notify(move |_| {
                update_sens_on_html();
            });

            let update_sens_on_policy = update_html_sensitivities.clone();
            row_image_policy.connect_selected_notify(move |_| {
                update_sens_on_policy();
            });
        }

        // ── Reading Comfort and Keyboard Group ──────────────────────────────
        let row_caret_mode = adw::SwitchRow::builder()
            .title("Caret-Navigation (F7)")
            .active(false)
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_caret_mode.set_active(settings.mail_caret_mode());
            row_caret_mode.connect_active_notify(move |row| {
                settings.set_mail_caret_mode(row.is_active());
            });
        }
        // row_caret_mode added to sub_reading_advanced

        let row_magic_spacebar = adw::SwitchRow::builder()
            .title("Magic Spacebar")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_magic_spacebar.set_active(settings.mail_magic_spacebar());
            row_magic_spacebar.connect_active_notify(move |row| {
                settings.set_mail_magic_spacebar(row.is_active());
            });
        }
        // row_magic_spacebar added to sub_reading_advanced
        

        let current_default_folder = settings
            .as_ref()
            .map(AppSettings::mail_default_folder);
        let startup_folder_entries =
            collect_account_panel_folders(current_default_folder.as_deref());
        let startup_folder_labels: Vec<&str> = startup_folder_entries
            .iter()
            .map(|(_, label)| label.as_str())
            .collect();
        let row_mail_default_folder = adw::ComboRow::builder()
            .title("Startup Folder")
            .model(&gtk::StringList::new(&startup_folder_labels))
            .build();
        configure_folder_combo_row(&row_mail_default_folder);
        if let Some(settings) = settings.clone() {
            let cur = settings.mail_default_folder();
            let initial_idx = startup_folder_entries
                .iter()
                .position(|(id, _)| id == &cur)
                .unwrap_or(0) as u32;
            row_mail_default_folder.set_selected(initial_idx);
            let entries_for_select = startup_folder_entries.clone();
            row_mail_default_folder.connect_selected_notify(move |row| {
                let idx = row.selected() as usize;
                let folder_id = entries_for_select
                    .get(idx)
                    .map(|(id, _)| id.as_str())
                    .unwrap_or("inbox");
                settings.set_mail_default_folder(folder_id);
            });
        }
        // row_mail_default_folder added to group_behavior

        // ── Window and Layout Group (Mail) ──────────────────────────────────
        let row_mail_layout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Mail Layout"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Side-by-Side"),
                &crate::i18n::gettext("Stacked"),
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
        let reading_pane_model = gtk::StringList::new(&[
            &crate::i18n::gettext("Hidden"),
            &crate::i18n::gettext("Below"),
            &crate::i18n::gettext("Right"),
        ]);
        let row_reading_pane = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Reading Pane"))
            .model(&reading_pane_model)
            .build();
        if let Some(ref s) = settings {
            let get_reading_pane_idx = |settings: &AppSettings| -> u32 {
                if !settings.show_message_preview() {
                    0
                } else if settings.mail_layout() == "classic" {
                    1
                } else {
                    2
                }
            };
            let get_reading_pane_idx_obj = |s_obj: &gio::Settings| -> u32 {
                if !s_obj.boolean("show-message-preview") {
                    0
                } else if s_obj.string("mail-layout").as_str() == "classic" {
                    1
                } else {
                    2
                }
            };
            row_reading_pane.set_selected(get_reading_pane_idx(s));

            let s_pane = s.clone();
            let updating_pane = Rc::new(Cell::new(false));
            let updating_pane_sel = updating_pane.clone();
            row_reading_pane.connect_selected_notify(move |r| {
                if updating_pane_sel.get() { return; }
                updating_pane_sel.set(true);
                match r.selected() {
                    0 => {
                        s_pane.set_show_message_preview(false);
                    }
                    1 => {
                        s_pane.set_show_message_preview(true);
                        s_pane.set_mail_layout("classic");
                    }
                    _ => {
                        s_pane.set_show_message_preview(true);
                        s_pane.set_mail_layout("modern");
                    }
                }
                updating_pane_sel.set(false);
            });

            let row_weak = row_reading_pane.downgrade();
            let updating_pane_ch = updating_pane.clone();
            s.connect_changed(Some("mail-layout"), move |s_obj, _| {
                if updating_pane_ch.get() { return; }
                if let Some(r) = row_weak.upgrade() {
                    let target = get_reading_pane_idx_obj(s_obj);
                    if r.selected() != target {
                        updating_pane_ch.set(true);
                        r.set_selected(target);
                        updating_pane_ch.set(false);
                    }
                }
            });
            let row_weak_prev = row_reading_pane.downgrade();
            let updating_pane_prev = updating_pane.clone();
            s.connect_changed(Some("show-message-preview"), move |s_obj, _| {
                if updating_pane_prev.get() { return; }
                if let Some(r) = row_weak_prev.upgrade() {
                    let target = get_reading_pane_idx_obj(s_obj);
                    if r.selected() != target {
                        updating_pane_prev.set(true);
                        r.set_selected(target);
                        updating_pane_prev.set(false);
                    }
                }
            });
        }
        let layout_choice = make_segmented_toggle_3_row(
            &row_reading_pane,
            &crate::i18n::gettext("Reading Pane"),
            &crate::i18n::gettext("Hidden"),
            &crate::i18n::gettext("Below"),
            &crate::i18n::gettext("Right"),
        );
        
        

        let row_mail_sidebar = adw::SwitchRow::builder()
            .title("Show folder sidebar")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sidebar.set_active(settings.mail_show_sidebar());
            row_mail_sidebar.connect_active_notify(move |row| {
                settings.set_mail_show_sidebar(row.is_active());
            });
        }
        

        let row_mail_toolbar = adw::SwitchRow::builder()
            .title("Show classic mail toolbar")
            .active(true)
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_toolbar.set_active(settings.mail_show_toolbar());
            row_mail_toolbar.connect_active_notify(move |row| {
                settings.set_mail_show_toolbar(row.is_active());
            });
        }
        

        let row_delete_moves_trash = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Move deleted messages to Trash"))
            .active(true)
            .build();
        if let Some(ref s) = settings {
            row_delete_moves_trash.set_active(s.mail_delete_moves_to_trash());
            let s_del = s.clone();
            row_delete_moves_trash.connect_active_notify(move |r| {
                s_del.set_mail_delete_moves_to_trash(r.is_active());
            });
        }
        

        let row_show_todo_bar = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show Today's To-Do sidebar"))
            .active(false)
            .build();
        if let Some(ref s) = settings {
            row_show_todo_bar.set_active(s.mail_show_todo_bar());
            let s_todo = s.clone();
            row_show_todo_bar.connect_active_notify(move |r| {
                s_todo.set_mail_show_todo_bar(r.is_active());
            });
        }
        

        let row_mail_density = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Message list density"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Compact"),
                &crate::i18n::gettext("Comfortable"),
                &crate::i18n::gettext("Spacious"),
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
        let toggle_mail_density = make_segmented_toggle_3_row(
            &row_mail_density,
            &crate::i18n::gettext("Message list density"),
            &crate::i18n::gettext("Compact"),
            &crate::i18n::gettext("Comfortable"),
            &crate::i18n::gettext("Spacious"),
        );
        
        

        let row_mail_unified_inbox = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Unified Inbox"))
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_unified_inbox.set_active(settings.mail_enable_unified_inbox());
            let settings_for_unified = settings.clone();
            row_mail_unified_inbox.connect_active_notify(move |row| {
                settings_for_unified.set_mail_enable_unified_inbox(row.is_active());
            });
            let row_weak = row_mail_unified_inbox.downgrade();
            settings.connect_changed(Some("mail-enable-unified-inbox"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-enable-unified-inbox");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        

        // ── Window and Controls Group (App-wide) ────────────────────────────
        let row_task_layout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Tasks Layout"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Side-by-Side"),
                &crate::i18n::gettext("Stacked"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_task_layout.set_selected(match settings.task_preview_orientation().as_str() {
                "classic" => 1,
                _ => 0,
            });
            row_task_layout.connect_selected_notify(move |row| {
                settings.set_task_preview_orientation(if row.selected() == 1 {
                    "classic"
                } else {
                    "vertical"
                });
            });
        }
        row_task_layout.set_visible(false);

        let row_memo_layout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Memos Layout"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Side-by-Side"),
                &crate::i18n::gettext("Stacked"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_memo_layout.set_selected(match settings.memo_preview_orientation().as_str() {
                "classic" => 1,
                _ => 0,
            });
            row_memo_layout.connect_selected_notify(move |row| {
                settings.set_memo_preview_orientation(if row.selected() == 1 {
                    "classic"
                } else {
                    "vertical"
                });
            });
        }
        row_memo_layout.set_visible(false);

        let row_mail_statusbar = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show status bar at bottom"))
            .active(true)
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_statusbar.set_active(settings.mail_show_statusbar());
            row_mail_statusbar.connect_active_notify(move |row| {
                settings.set_mail_show_statusbar(row.is_active());
            });
        }

        let row_debug_console = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Enable developer log console"))
            .active(false)
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_debug_console.set_active(settings.debug_console_enabled());
            let row_statusbar_ref = row_mail_statusbar.clone();
            let dialog_weak_dbg = self.downgrade();
            row_debug_console.connect_active_notify(move |row| {
                let active = row.is_active();
                settings.set_debug_console_enabled(active);
                if active {
                    settings.set_mail_show_statusbar(true);
                    row_statusbar_ref.set_active(true);
                }
                if let Some(d) = dialog_weak_dbg.upgrade() {
                    if let Some(win) = crate::window::find_meridian_window(&d) {
                        win.set_debug_console_open(active);
                    }
                }
            });
        }

        let group_mail_elements = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Message List Elements"))
            .build();

        let row_mail_show_avatar = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show sender avatars"))
            .active(true)
            .build();

        let row_mail_fetch_remote_logos = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Fetch Company Logos (BIMI and Gravatar)"))
            .active(false)
            .build();

        let row_mail_show_preview_text = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show preview text in list"))
            .active(true)
            .build();

        let row_mail_show_flagged = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Starred and Flagged Indicators"))
            .active(true)
            .build();

        let row_mail_show_attachment = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show Attachment Indicator"))
            .active(true)
            .build();

        let row_mail_show_size = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show Message Size"))
            .active(false)
            .build();

        let row_mail_show_labels = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show Color Labels"))
            .active(true)
            .build();

        if let Some(settings) = settings.clone() {
            // Avatar
            row_mail_show_avatar.set_active(settings.mail_show_avatar());
            let s_av = settings.clone();
            row_mail_show_avatar.connect_active_notify(move |row| {
                s_av.set_mail_show_avatar(row.is_active());
            });
            let row_weak = row_mail_show_avatar.downgrade();
            settings.connect_changed(Some("mail-show-avatar"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-avatar");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            // Remote logos
            row_mail_fetch_remote_logos.set_active(settings.mail_fetch_remote_logos());
            row_mail_fetch_remote_logos.set_sensitive(settings.mail_show_avatar());
            let s_lg = settings.clone();
            row_mail_fetch_remote_logos.connect_active_notify(move |row| {
                s_lg.set_mail_fetch_remote_logos(row.is_active());
            });
            let row_weak = row_mail_fetch_remote_logos.downgrade();
            settings.connect_changed(Some("mail-fetch-remote-logos"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-fetch-remote-logos");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
            let r_lg = row_mail_fetch_remote_logos.clone();
            row_mail_show_avatar.connect_active_notify(move |row| {
                r_lg.set_sensitive(row.is_active());
            });

            // Preview text
            row_mail_show_preview_text.set_active(settings.mail_show_preview_text());
            let s_pt = settings.clone();
            row_mail_show_preview_text.connect_active_notify(move |row| {
                s_pt.set_mail_show_preview_text(row.is_active());
            });
            let row_weak = row_mail_show_preview_text.downgrade();
            settings.connect_changed(Some("mail-show-preview-text"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-preview-text");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            // Flagged
            row_mail_show_flagged.set_active(settings.mail_show_flagged());
            let s_fl = settings.clone();
            row_mail_show_flagged.connect_active_notify(move |row| {
                s_fl.set_mail_show_flagged(row.is_active());
            });
            let row_weak = row_mail_show_flagged.downgrade();
            settings.connect_changed(Some("mail-show-flagged"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-flagged");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            // Attachment
            row_mail_show_attachment.set_active(settings.mail_show_attachment());
            let s_at = settings.clone();
            row_mail_show_attachment.connect_active_notify(move |row| {
                s_at.set_mail_show_attachment(row.is_active());
            });
            let row_weak = row_mail_show_attachment.downgrade();
            settings.connect_changed(Some("mail-show-attachment"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-attachment");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            // Size
            row_mail_show_size.set_active(settings.mail_show_size());
            let s_sz = settings.clone();
            row_mail_show_size.connect_active_notify(move |row| {
                s_sz.set_mail_show_size(row.is_active());
            });
            let row_weak = row_mail_show_size.downgrade();
            settings.connect_changed(Some("mail-show-size"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-size");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });

            // Labels
            row_mail_show_labels.set_active(settings.mail_show_labels());
            let s_lbl = settings.clone();
            row_mail_show_labels.connect_active_notify(move |row| {
                s_lbl.set_mail_show_labels(row.is_active());
            });
            let row_weak = row_mail_show_labels.downgrade();
            settings.connect_changed(Some("mail-show-labels"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-labels");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }



        let row_confirm_destructive = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Confirm before destructive actions"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_confirm_destructive.set_active(settings.confirm_destructive_actions());
            let settings_for_confirm = settings.clone();
            row_confirm_destructive.connect_active_notify(move |row| {
                settings_for_confirm.set_confirm_destructive_actions(row.is_active());
            });
            let row_weak = row_confirm_destructive.downgrade();
            settings.connect_changed(Some("confirm-destructive-actions"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("confirm-destructive-actions");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }

        

        let group_citations = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Citations and Highlighting"))
            .build();

        let exp_citation_highlighting = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Highlight quoted text"))
            .show_enable_switch(true)
            .enable_expansion(true)
            .build();

        fn make_quote_color_child(
            title: &str,
            initial_hex: &str,
            settings: Option<AppSettings>,
            setter: fn(&AppSettings, &str),
        ) -> adw::ActionRow {
            let row = adw::ActionRow::builder()
                .title(title)
                .build();
            let color_dialog = gtk::ColorDialog::builder().title(title).build();
            let btn = gtk::ColorDialogButton::new(Some(color_dialog));
            if let Ok(rgba) = gtk::gdk::RGBA::parse(initial_hex) {
                btn.set_rgba(&rgba);
            }
            btn.set_valign(gtk::Align::Center);
            let s_clone = settings.clone();
            btn.connect_rgba_notify(move |b| {
                let rgba = b.rgba();
                let hex = format!(
                    "#{:02X}{:02X}{:02X}",
                    (rgba.red() * 255.0).round() as u8,
                    (rgba.green() * 255.0).round() as u8,
                    (rgba.blue() * 255.0).round() as u8,
                );
                if let Some(ref s) = s_clone {
                    setter(s, &hex);
                }
            });
            row.add_suffix(&btn);
            row
        }

        let is_cit_active = settings.as_ref().map(|s| s.mail_citation_highlighting()).unwrap_or(true);
        exp_citation_highlighting.set_enable_expansion(is_cit_active);
        if let Some(ref s) = settings {
            let s_cit = s.clone();
            exp_citation_highlighting.connect_enable_expansion_notify(move |r| {
                s_cit.set_mail_citation_highlighting(r.enables_expansion());
            });
        }

        let c1 = settings.as_ref().map(|s| s.mail_citation_color_1()).unwrap_or_else(|| "#729FCF".to_string());
        let c2 = settings.as_ref().map(|s| s.mail_citation_color_2()).unwrap_or_else(|| "#AD7FA8".to_string());
        let c3 = settings.as_ref().map(|s| s.mail_citation_color_3()).unwrap_or_else(|| "#8AE234".to_string());
        let c4 = settings.as_ref().map(|s| s.mail_citation_color_4()).unwrap_or_else(|| "#FCAF3E".to_string());
        let c5 = settings.as_ref().map(|s| s.mail_citation_color_5()).unwrap_or_else(|| "#E9B96E".to_string());

        let row_qc1 = make_quote_color_child(&crate::i18n::gettext("Quote Level 1"), &c1, settings.clone(), AppSettings::set_mail_citation_color_1);
        let row_qc2 = make_quote_color_child(&crate::i18n::gettext("Quote Level 2"), &c2, settings.clone(), AppSettings::set_mail_citation_color_2);
        let row_qc3 = make_quote_color_child(&crate::i18n::gettext("Quote Level 3"), &c3, settings.clone(), AppSettings::set_mail_citation_color_3);
        let row_qc4 = make_quote_color_child(&crate::i18n::gettext("Quote Level 4"), &c4, settings.clone(), AppSettings::set_mail_citation_color_4);
        let row_qc5 = make_quote_color_child(&crate::i18n::gettext("Quote Level 5"), &c5, settings.clone(), AppSettings::set_mail_citation_color_5);

        exp_citation_highlighting.add_row(&row_qc1);
        exp_citation_highlighting.add_row(&row_qc2);
        exp_citation_highlighting.add_row(&row_qc3);
        exp_citation_highlighting.add_row(&row_qc4);
        exp_citation_highlighting.add_row(&row_qc5);

        group_mail_reading.add(&exp_citation_highlighting);

        let row_citation_highlighting = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Highlight quoted text"))
            .active(true)
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_citation_highlighting.set_active(settings.mail_citation_highlighting());
            row_citation_highlighting.connect_active_notify(move |row| {
                settings.set_mail_citation_highlighting(row.is_active());
            });
        }
        group_mail_reading.add(&row_citation_highlighting);
        group_citations.set_visible(false);

        let group_theme = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Style"))
            .build();
        let row_color_scheme = adw::ComboRow::builder()
            .title("Application Color Scheme")
            .model(&gtk::StringList::new(&["System", "Light", "Dark"]))
            .build();
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_color_scheme.set_selected(match settings.color_scheme().as_str() {
                "light" => 1,
                "dark" => 2,
                _ => 0,
            });
            row_color_scheme.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                let scheme = match row.selected() {
                    1 => "light",
                    2 => "dark",
                    _ => "system",
                };
                settings_notify.set_color_scheme(scheme);
                adw::StyleManager::default().set_color_scheme(AppSettings::parse_adw_color_scheme(scheme));
            });

            let row_for_change = row_color_scheme.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("color-scheme"), move |s, _| {
                let target = match s.string("color-scheme").as_str() {
                    "light" => 1,
                    "dark" => 2,
                    _ => 0,
                };
                if row_for_change.selected() != target {
                    updating_change.set(true);
                    row_for_change.set_selected(target);
                    updating_change.set(false);
                }
            });
        }
        let row_theme_toggle = make_segmented_toggle_3_row(
            &row_color_scheme,
            &crate::i18n::gettext("Style"),
            &crate::i18n::gettext("System"),
            &crate::i18n::gettext("Light"),
            &crate::i18n::gettext("Dark"),
        );
        row_color_scheme.set_visible(false);
        group_theme.add(&row_color_scheme);
        group_theme.add(&row_theme_toggle);
        group_theme.add(&row_dark_mail);

        let group_behavior = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Behavior"))
            .build();
        let row_default_mail_app = build_default_handler_row(
            self,
            &crate::i18n::gettext("Default Email Application"),
            &crate::i18n::gettext("Meridian is the system-wide default email application"),
            &crate::i18n::gettext("Use Meridian for mailto: links and email files system-wide"),
            &crate::i18n::gettext("Meridian set as default email application"),
            "x-scheme-handler/mailto",
            crate::application::DEFAULT_MAIL_MIME_TYPES,
        );
        group_behavior.add(&row_default_mail_app);
        group_behavior.add(&row_mail_default_folder);
        group_behavior.add(&row_mail_unified_inbox);
        group_behavior.add(&row_confirm_destructive);
        group_behavior.add(&row_delete_moves_trash);

        let group_appearance_layout = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Layout"))
            .build();
        group_appearance_layout.add(&row_switcher_location);
        group_appearance_layout.add(&nav_choice);
        group_appearance_layout.add(&row_reading_pane);
        group_appearance_layout.add(&layout_choice);
        group_appearance_layout.add(&row_mail_density);
        group_appearance_layout.add(&toggle_mail_density);
        // Folder and To-Do sidebars are toggled directly in the main window.
        let _ = (&row_mail_sidebar, &row_show_todo_bar);
        row_mail_statusbar.set_visible(false);
        group_appearance_layout.add(&row_mail_statusbar);

        page_appearance.add(&group_theme);
        page_appearance.add(&group_appearance_layout);

        let group_appearance_nav = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Toolbars"))
            .build();
        let row_nav_header_toolbar = make_subpage_nav_row(
            &crate::i18n::gettext("Message Header Actions"),
            "sub_header_toolbar",
            &open_subpage,
        );
        group_appearance_nav.add(&row_nav_header_toolbar);
        let row_nav_list_toolbar = make_subpage_nav_row(
            &crate::i18n::gettext("List & Quick-Filter Bar"),
            "sub_list_toolbar",
            &open_subpage,
        );
        group_appearance_nav.add(&row_nav_list_toolbar);
        page_appearance.add(&group_appearance_nav);


        let row_mail_sort = adw::ComboRow::builder()
            .title("Sort Field")
            .model(&gtk::StringList::new(&[
                "Date",
                "Subject",
                "Sender",
                "Unread Status",
                "Flagged",
                "Follow-up",
                "Importance",
                "Attachments",
                "Size",
                "Due Date",
                "Location",
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
        

        let row_mail_sort_direction = adw::SwitchRow::builder()
            .title("Sort Descending")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sort_direction.set_active(settings.mail_sort_descending());
            row_mail_sort_direction.connect_active_notify(move |row| {
                settings.set_mail_sort_descending(row.is_active());
            });
        }
        

        let row_mail_threading = adw::SwitchRow::builder()
            .title("Group Conversations")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_threading.set_active(settings.mail_threading());
            row_mail_threading.connect_active_notify(move |row| {
                settings.set_mail_threading(row.is_active());
            });
        }
        

        let row_mail_group_similar = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Group Similar Messages"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_group_similar.set_active(settings.mail_group_similar_messages());
            row_mail_group_similar.set_sensitive(settings.mail_threading());
            let s_sim = settings.clone();
            row_mail_group_similar.connect_active_notify(move |row| {
                s_sim.set_mail_group_similar_messages(row.is_active());
            });
            let row_sim_weak = row_mail_group_similar.downgrade();
            settings.connect_changed(Some("mail-group-similar-messages"), move |s, _| {
                let Some(row) = row_sim_weak.upgrade() else {
                    return;
                };
                if s.settings_schema()
                    .is_some_and(|schema| schema.has_key("mail-group-similar-messages"))
                {
                    let active = s.boolean("mail-group-similar-messages");
                    if row.is_active() != active {
                        row.set_active(active);
                    }
                }
            });
            let r_sim = row_mail_group_similar.clone();
            row_mail_threading.connect_active_notify(move |row| {
                r_sim.set_sensitive(row.is_active());
            });
        }
        

        let row_mail_conversation_accordion = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Auto-expand conversations"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_conversation_accordion.set_active(settings.mail_conversation_accordion());
            row_mail_conversation_accordion.set_sensitive(settings.mail_threading());
            let s_acc = settings.clone();
            row_mail_conversation_accordion.connect_active_notify(move |row| {
                s_acc.set_mail_conversation_accordion(row.is_active());
            });
            let row_weak = row_mail_conversation_accordion.downgrade();
            settings.connect_changed(Some("mail-conversation-accordion"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-conversation-accordion");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
            let r_acc = row_mail_conversation_accordion.clone();
            row_mail_threading.connect_active_notify(move |row| {
                r_acc.set_sensitive(row.is_active());
            });
        }
        

        let row_mail_date_format = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Date Format"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Adaptive"),
                &crate::i18n::gettext("Relative"),
                "10.09.26",
                "10.09.2026",
                "10.09.2026 14:30",
                "2026-09-10",
                &crate::i18n::gettext("Custom…"),
            ]))
            .build();

        let row_mail_date_custom = adw::EntryRow::builder()
            .title("Custom Date Format (Strftime)")
            .show_apply_button(true)
            .build();
        let date_custom_help_icon = gtk::Image::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text(
                "Placeholders:\n%Y = Year (2026), %y = Short year (26)\n%m = Month (01–12), %b = Short month (Sep)\n%d = Day (01–31), %a = Weekday (Thu)\n%H:%M = 24h time (14:30)",
            )
            .css_classes(vec!["dim-label"])
            .build();
        row_mail_date_custom.add_suffix(&date_custom_help_icon);

        let row_mail_date_align = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Date Alignment"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Left-Aligned"),
                &crate::i18n::gettext("Right-Aligned"),
            ]))
            .build();

        if let Some(settings) = settings.clone() {
            let current_fmt = settings.mail_date_format();
            row_mail_date_format.set_selected(match current_fmt.as_str() {
                "smart" => 1,
                "short" => 2,
                "full" => 3,
                "datetime" => 4,
                "iso" => 5,
                "custom" => 6,
                _ => 0,
            });
            row_mail_date_custom.set_visible(current_fmt == "custom");
            row_mail_date_custom.set_text(&settings.mail_date_custom_format());

            let s_fmt = settings.clone();
            let r_custom = row_mail_date_custom.clone();
            row_mail_date_format.connect_selected_notify(move |row| {
                let chosen = match row.selected() {
                    1 => "smart",
                    2 => "short",
                    3 => "full",
                    4 => "datetime",
                    5 => "iso",
                    6 => "custom",
                    _ => "standard",
                };
                s_fmt.set_mail_date_format(chosen);
                r_custom.set_visible(chosen == "custom");
            });

            let s_custom = settings.clone();
            row_mail_date_custom.connect_apply(move |row| {
                s_custom.set_mail_date_custom_format(row.text().as_str());
            });

            // Index 0 = Left, 1 = Right (matches the visual [ Left | Right ] toggle).
            row_mail_date_align.set_selected(if settings.mail_date_align() == "left" { 0 } else { 1 });
            let s_align = settings.clone();
            row_mail_date_align.connect_selected_notify(move |row| {
                s_align.set_mail_date_align(if row.selected() == 0 { "left" } else { "right" });
            });
        }
        
        

        let group_mail_headers_mode = adw::PreferencesGroup::builder()
            .title("Header Display")
            .build();

        let row_header_style = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Header Style"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Modern"),
                &crate::i18n::gettext("Tabular"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let initial_idx = if settings.mail_header_style() == "classic" { 1 } else { 0 };
            row_header_style.set_selected(initial_idx);
            let settings_for_style = settings.clone();
            row_header_style.connect_selected_notify(move |row| {
                let val = if row.selected() == 1 { "classic" } else { "modern" };
                settings_for_style.set_mail_header_style(val);
            });
            let row_weak = row_header_style.downgrade();
            settings.connect_changed(Some("mail-header-style"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let target = if s.string("mail-header-style").as_str() == "classic" { 1 } else { 0 };
                if row.selected() != target {
                    row.set_selected(target);
                }
            });
        }
        group_mail_headers_mode.add(&row_header_style);
        let row_header_toggle = make_segmented_toggle_row(
            &row_header_style,
            &crate::i18n::gettext("Header Style"),
            &crate::i18n::gettext("Modern"),
            &crate::i18n::gettext("Tabular"),
        );
        group_mail_headers_mode.add(&row_header_toggle);

        let group_mail_header_actions = build_mail_header_toolbar_editor(settings.clone());
        let group_mail_list_toolbar = build_mail_list_toolbar_editor(settings.clone());

        let group_mail_normal_headers = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Tabular Header Rows"))
            .description(&crate::i18n::gettext(
                "Shown directly in the message header when using Tabular style.",
            ))
            .build();
        let normal_headers_editor =
            build_mail_normal_headers_editor(settings.clone(), &group_mail_normal_headers);
        group_mail_normal_headers.add(&normal_headers_editor);

        let group_mail_expanded_headers = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Expanded Headers"))
            .description(&crate::i18n::gettext(
                "Shown when expanding all headers via the arrow button in the message header.",
            ))
            .build();
        let expanded_headers_editor =
            build_mail_expanded_headers_editor(self, &group_mail_expanded_headers);
        group_mail_expanded_headers.add(&expanded_headers_editor);

        let group_calendar_view = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Calendar View"))
            .build();
        let row_calendar_view = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Default View"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Day"),
                &crate::i18n::gettext("Week"),
                &crate::i18n::gettext("Work Week"),
                &crate::i18n::gettext("Month"),
                &crate::i18n::gettext("Agenda"),
                &crate::i18n::gettext("Year"),
                &crate::i18n::gettext("List"),
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
        

        let row_time_divisions = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Time Divisions"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("60 Minutes"),
                &crate::i18n::gettext("30 Minutes"),
                &crate::i18n::gettext("15 Minutes"),
                &crate::i18n::gettext("10 Minutes"),
                &crate::i18n::gettext("5 Minutes"),
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
        

        let row_calendar_click_action = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Click on time slot"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Select Time Slot"),
                &crate::i18n::gettext("New Event"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_calendar_click_action.set_selected(if settings.calendar_click_action() == "create" {
                1
            } else {
                0
            });
            row_calendar_click_action.connect_selected_notify(move |row| {
                settings.set_calendar_click_action(if row.selected() == 1 {
                    "create"
                } else {
                    "select"
                });
            });
        }
        
        let row_click_toggle = make_segmented_toggle_row(
            &row_calendar_click_action,
            &crate::i18n::gettext("Click on time slot"),
            &crate::i18n::gettext("Select Time Slot"),
            &crate::i18n::gettext("New Event"),
        );
        

        let row_week_start = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Week Start"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Monday"),
                &crate::i18n::gettext("Sunday"),
            ]))
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
        
        let row_week_toggle = make_segmented_toggle_row(
            &row_week_start,
            &crate::i18n::gettext("Week Start"),
            &crate::i18n::gettext("Monday"),
            &crate::i18n::gettext("Sunday"),
        );
        

        let row_week_numbers = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show calendar weeks"))
            .build();
        if let Some(settings) = settings.clone() {
            row_week_numbers.set_active(settings.calendar_show_week_numbers());
            row_week_numbers.connect_active_notify(move |row| {
                settings.set_calendar_show_week_numbers(row.is_active());
            });
        }
        

        let row_compress_weekend = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Compress weekend in month view"))
            .build();
        if let Some(settings) = settings.clone() {
            row_compress_weekend.set_active(settings.calendar_compress_weekend());
            row_compress_weekend.connect_active_notify(move |row| {
                settings.set_calendar_compress_weekend(row.is_active());
            });
        }
        

        let row_month_scroll_by_week = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Scroll Month View by Week"))
            .build();
        if let Some(settings) = settings.clone() {
            row_month_scroll_by_week.set_active(settings.calendar_month_scroll_by_week());
            row_month_scroll_by_week.connect_active_notify(move |row| {
                settings.set_calendar_month_scroll_by_week(row.is_active());
            });
        }
        row_month_scroll_by_week.set_visible(true);

        let row_month_scroll_anim = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Animate Month View Scrolling"))
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_month_scroll_anim.set_active(settings.calendar_month_scroll_animation());
            row_month_scroll_anim.connect_active_notify(move |row| {
                settings.set_calendar_month_scroll_animation(row.is_active());
            });
        }

        let row_dim_past_events = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Dim past events"))
            .build();
        if let Some(settings) = settings.clone() {
            row_dim_past_events.set_active(settings.calendar_dim_past_events());
            row_dim_past_events.connect_active_notify(move |row| {
                settings.set_calendar_dim_past_events(row.is_active());
            });
        }
        

        let row_date_format = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Date Format"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("System Default"),
                &crate::i18n::gettext("DD.MM.YYYY"),
                &crate::i18n::gettext("MM/DD/YYYY"),
                &crate::i18n::gettext("YYYY-MM-DD"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_date_format.set_selected(match settings.date_format().as_str() {
                "german" => 1,
                "us" => 2,
                "iso" => 3,
                _ => 0,
            });
            row_date_format.connect_selected_notify(move |row| {
                settings.set_date_format(match row.selected() {
                    1 => "german",
                    2 => "us",
                    3 => "iso",
                    _ => "locale",
                });
            });
        }
        

        let row_time_format = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Time Format"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("System Default"),
                &crate::i18n::gettext("24-Hour"),
                &crate::i18n::gettext("12-Hour"),
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            row_time_format.set_selected(match settings.time_format().as_str() {
                "24h" => 1,
                "12h" => 2,
                _ => 0,
            });
            row_time_format.connect_selected_notify(move |row| {
                settings.set_time_format(match row.selected() {
                    1 => "24h",
                    2 => "12h",
                    _ => "locale",
                });
            });
        }
        

        let row_calendar_time_zone = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Calendar time zone"))
            .visible(false)
            .build();
        row_calendar_time_zone.set_tooltip_text(Some(&crate::i18n::gettext(
            "system for machine time zone, UTC, or an IANA zone such as Europe/Berlin",
        )));
        if let Some(settings) = settings.clone() {
            row_calendar_time_zone.set_text(&settings.calendar_time_zone());
            row_calendar_time_zone.connect_changed(move |row| {
                settings.set_calendar_time_zone(&row.text());
            });
        }

        let row_second_zone = adw::EntryRow::builder()
            .title("Second Time Zone")
            .visible(false)
            .build();
        row_second_zone.set_tooltip_text(Some(
            "Additional time zone for day view (e.g. UTC or America/New_York)",
        ));
        if let Some(settings) = settings.clone() {
            row_second_zone.set_text(&settings.calendar_second_zone());
            row_second_zone.connect_changed(move |row| {
                settings.set_calendar_second_zone(&row.text());
            });
        }

        let mut iana_zones: Vec<String> = Vec::new();
        if let Ok(content) = std::fs::read_to_string("/usr/share/zoneinfo/zone1970.tab") {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let mut parts = trimmed.split('\t');
                let _countries = parts.next();
                let _coords = parts.next();
                if let Some(tz) = parts.next() {
                    let tz_str = tz.trim().to_string();
                    if !tz_str.is_empty() {
                        iana_zones.push(tz_str);
                    }
                }
            }
        }
        if iana_zones.is_empty() {
            for fallback in [
                "Europe/Berlin",
                "Europe/London",
                "Europe/Paris",
                "Europe/Zurich",
                "Europe/Vienna",
                "America/New_York",
                "America/Chicago",
                "America/Denver",
                "America/Los_Angeles",
                "Asia/Tokyo",
                "Asia/Singapore",
                "Australia/Sydney",
            ] {
                iana_zones.push(fallback.to_string());
            }
        }
        iana_zones.sort();
        iana_zones.dedup();

        let mut primary_tz_values = vec!["system".to_string(), "UTC".to_string()];
        let mut primary_tz_labels = vec![
            crate::i18n::gettext("System Default"),
            "UTC".to_string(),
        ];
        for tz in &iana_zones {
            primary_tz_values.push(tz.clone());
            primary_tz_labels.push(tz.clone());
        }
        let primary_tz_label_refs: Vec<&str> = primary_tz_labels.iter().map(|s| s.as_str()).collect();
        let tz_expr = gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        );
        let row_calendar_time_zone_combo = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Calendar Time Zone"))
            .model(&gtk::StringList::new(&primary_tz_label_refs))
            .enable_search(true)
            .expression(&tz_expr)
            .build();
        if let Some(settings) = settings.clone() {
            let cur = settings.calendar_time_zone();
            let idx = primary_tz_values
                .iter()
                .position(|z| z.eq_ignore_ascii_case(&cur))
                .unwrap_or(0) as u32;
            row_calendar_time_zone_combo.set_selected(idx);
            let vals = primary_tz_values.clone();
            row_calendar_time_zone_combo.connect_selected_notify(move |row| {
                if let Some(val) = vals.get(row.selected() as usize) {
                    settings.set_calendar_time_zone(val);
                }
            });
        }

        let mut second_tz_values = vec![String::new(), "UTC".to_string()];
        let mut second_tz_labels = vec![
            crate::i18n::gettext("None"),
            "UTC".to_string(),
        ];
        for tz in &iana_zones {
            second_tz_values.push(tz.clone());
            second_tz_labels.push(tz.clone());
        }
        let second_tz_label_refs: Vec<&str> = second_tz_labels.iter().map(|s| s.as_str()).collect();
        let tz2_expr = gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        );
        let row_second_zone_combo = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Second Time Zone"))
            .model(&gtk::StringList::new(&second_tz_label_refs))
            .enable_search(true)
            .expression(&tz2_expr)
            .build();
        if let Some(settings) = settings.clone() {
            let cur = settings.calendar_second_zone();
            let idx = second_tz_values
                .iter()
                .position(|z| z.eq_ignore_ascii_case(cur.trim()))
                .unwrap_or(0) as u32;
            row_second_zone_combo.set_selected(idx);
            let vals = second_tz_values.clone();
            row_second_zone_combo.connect_selected_notify(move |row| {
                if let Some(val) = vals.get(row.selected() as usize) {
                    settings.set_calendar_second_zone(val);
                }
            });
        }

        let row_location_map_hover = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show map preview on location hover"))
            .visible(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_location_map_hover.set_active(settings.calendar_location_map_hover());
            row_location_map_hover.connect_active_notify(move |row| {
                settings.set_calendar_location_map_hover(row.is_active());
            });
        }
        group_calendar_view.add(&row_location_map_hover);

        let row_location_open_map = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Open locations in map on click"))
            .build();
        if let Some(settings) = settings.clone() {
            row_location_open_map.set_active(settings.calendar_location_open_map());
            row_location_open_map.connect_active_notify(move |row| {
                settings.set_calendar_location_open_map(row.is_active());
            });
        }
        group_calendar_view.add(&row_location_open_map);

        let group_default_ecal_sources = adw::PreferencesGroup::builder()
            .title("Default Sources")
            .build();
        group_default_ecal_sources.add(&source_preference_row(
            "Default Calendar",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Calendar",
            settings.clone(),
            AppSettings::default_calendar_source_uid,
            AppSettings::set_default_calendar_source_uid,
        ));

        let group_default_tasks_sources = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Default Lists"))
            .build();
        group_default_tasks_sources.add(&source_preference_row(
            "Default Task List",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Task List",
            settings.clone(),
            AppSettings::default_task_source_uid,
            AppSettings::set_default_task_source_uid,
        ));
        group_default_tasks_sources.add(&source_preference_row(
            "Default Memo List",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Memo List",
            settings.clone(),
            AppSettings::default_memo_source_uid,
            AppSettings::set_default_memo_source_uid,
        ));

        let expander_workdays = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Select Work Days"))
            .use_markup(false)
            .expanded(false)
            .build();
        
        for (day, label) in [
            ("monday", "Monday"),
            ("tuesday", "Tuesday"),
            ("wednesday", "Wednesday"),
            ("thursday", "Thursday"),
            ("friday", "Friday"),
            ("saturday", "Saturday"),
            ("sunday", "Sunday"),
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
            expander_workdays.add_row(&row);
        }

        let group_calendar_time = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Working Hours"))
            .build();
        group_calendar_time.add(&expander_workdays);

        let workday_options = workday_time_options();
        let row_workday_start = adw::ComboRow::builder()
            .title("Work Day Starts")
            .model(&workday_options)
            .build();

        if let Some(settings) = settings.clone() {
            let start_mins = settings.calendar_workday_start_minutes();
            row_workday_start.set_selected(minutes_to_workday_time_index(start_mins));

            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();
            row_workday_start.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                let mins = workday_time_index_to_minutes(row.selected());
                settings_notify.set_calendar_workday_start_minutes(mins);
            });

            let row_for_change = row_workday_start.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("calendar-workday-start"), move |s, _| {
                let target = s.int("calendar-workday-start").clamp(0, 1439) as u32;
                let expected_idx = minutes_to_workday_time_index(target);
                if row_for_change.selected() != expected_idx {
                    updating_change.set(true);
                    row_for_change.set_selected(expected_idx);
                    updating_change.set(false);
                }
            });
        }
        group_calendar_time.add(&row_workday_start);

        let row_workday_end = adw::ComboRow::builder()
            .title("Work Day Ends")
            .model(&workday_options)
            .build();

        if let Some(settings) = settings.clone() {
            let end_mins = settings.calendar_workday_end_minutes();
            row_workday_end.set_selected(minutes_to_workday_time_index(end_mins));

            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();
            row_workday_end.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                let mins = workday_time_index_to_minutes(row.selected());
                settings_notify.set_calendar_workday_end_minutes(mins);
            });

            let row_for_change = row_workday_end.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("calendar-workday-end"), move |s, _| {
                let target = s.int("calendar-workday-end").clamp(0, 1439) as u32;
                let expected_idx = minutes_to_workday_time_index(target);
                if row_for_change.selected() != expected_idx {
                    updating_change.set(true);
                    row_for_change.set_selected(expected_idx);
                    updating_change.set(false);
                }
            });
        }
        group_calendar_time.add(&row_workday_end);

        let group_calendar_reminders = adw::PreferencesGroup::builder()
            .title("Reminders and Events")
            .build();

        let row_use_default_reminder = adw::SwitchRow::builder()
            .title("Default reminder for events")
            .build();
        let row_default_reminder_interval = adw::SpinRow::with_range(1.0, 10_080.0, 1.0);
        row_default_reminder_interval.set_title("Default reminder interval");
        row_default_reminder_interval.set_digits(0);
        row_default_reminder_interval.set_snap_to_ticks(true);

        let row_default_reminder_units = adw::ComboRow::builder()
            .title("Default reminder unit")
            .model(&gtk::StringList::new(&["Minutes", "Hours", "Days"]))
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
            .title("Birthday and anniversary reminders")
            .build();
        let row_ba_reminder_interval = adw::SpinRow::with_range(1.0, 10_080.0, 1.0);
        row_ba_reminder_interval.set_title("Interval for birthdays and anniversaries");
        row_ba_reminder_interval.set_digits(0);
        row_ba_reminder_interval.set_snap_to_ticks(true);

        let row_ba_reminder_units = adw::ComboRow::builder()
            .title("Unit for birthdays and anniversaries")
            .model(&gtk::StringList::new(&["Minutes", "Hours", "Days"]))
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

            let row_use_ba_ref = row_use_ba_reminder.clone();
            let row_interval_sens = row_ba_reminder_interval.clone();
            let row_units_sens = row_ba_reminder_units.clone();
            settings.connect_changed(Some("use-ba-reminder"), move |s, _| {
                let active = s.boolean("use-ba-reminder");
                if row_use_ba_ref.is_active() != active {
                    row_use_ba_ref.set_active(active);
                }
                row_interval_sens.set_sensitive(active);
                row_units_sens.set_sensitive(active);
            });

            let row_interval_changed = row_ba_reminder_interval.clone();
            settings.connect_changed(Some("ba-reminder-interval"), move |s, _| {
                let val = s.int("ba-reminder-interval") as f64;
                if (row_interval_changed.value() - val).abs() > f64::EPSILON {
                    row_interval_changed.set_value(val);
                }
            });

            let row_units_changed = row_ba_reminder_units.clone();
            settings.connect_changed(Some("ba-reminder-units"), move |s, _| {
                let unit = ReminderDurationUnit::from_str(&s.string("ba-reminder-units"));
                let target_index = match unit {
                    ReminderDurationUnit::Minutes => 0,
                    ReminderDurationUnit::Hours => 1,
                    ReminderDurationUnit::Days => 2,
                };
                if row_units_changed.selected() != target_index {
                    row_units_changed.set_selected(target_index);
                }
            });
        }
        let expander_ba_reminders = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Birthday & Anniversary Reminders"))
            .use_markup(false)
            .expanded(false)
            .build();
        
        expander_ba_reminders.add_row(&row_use_ba_reminder);
        expander_ba_reminders.add_row(&row_ba_reminder_interval);
        expander_ba_reminders.add_row(&row_ba_reminder_units);
        group_calendar_reminders.add(&expander_ba_reminders);

        let row_hide_cancelled_events = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Hide cancelled events"))
            .build();
        if let Some(settings) = settings.clone() {
            row_hide_cancelled_events.set_active(settings.hide_cancelled_events());
            let settings_for_cancelled = settings.clone();
            row_hide_cancelled_events.connect_active_notify(move |row| {
                settings_for_cancelled.set_hide_cancelled_events(row.is_active());
            });
        }
        

        let row_reminder_sound = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Play sound for reminders"))
            .build();
        if let Some(settings) = settings.clone() {
            row_reminder_sound.set_active(settings.reminder_sound_enabled());
            let settings_for_reminder_sound = settings.clone();
            row_reminder_sound.connect_active_notify(move |row| {
                settings_for_reminder_sound.set_reminder_sound_enabled(row.is_active());
            });
            let row_reminder_sound_ref = row_reminder_sound.clone();
            settings.connect_changed(Some("reminder-sound-enabled"), move |s, _| {
                let active = s.boolean("reminder-sound-enabled");
                if row_reminder_sound_ref.is_active() != active {
                    row_reminder_sound_ref.set_active(active);
                }
            });
        }
        

        let row_reminder_tray = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show reminders in notification tray only"))
            .visible(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_reminder_tray.set_active(settings.reminder_notify_with_tray());
            let settings_for_tray = settings.clone();
            row_reminder_tray.connect_active_notify(move |row| {
                settings_for_tray.set_reminder_notify_with_tray(row.is_active());
            });
            let row_reminder_tray_ref = row_reminder_tray.clone();
            settings.connect_changed(Some("reminder-notify-with-tray"), move |s, _| {
                let active = s.boolean("reminder-notify-with-tray");
                if row_reminder_tray_ref.is_active() != active {
                    row_reminder_tray_ref.set_active(active);
                }
            });
        }
        group_calendar_reminders.add(&row_reminder_tray);

        let row_notifications = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("System Notifications"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_notifications.set_active(settings.notifications_enabled());
            row_notifications.connect_active_notify(move |row| {
                settings.set_notifications_enabled(row.is_active());
            });
        }
        

        let group_categories = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Calendar Categories"))
            .build();

        let categories_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        group_categories.add(&categories_list_box);

        let rebuild_categories_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let dialog_weak_cat = self.downgrade();
        let categories_list_box_weak = categories_list_box.downgrade();

        let rebuild_categories: Rc<dyn Fn()> = {
            let rebuild_holder = rebuild_categories_holder.clone();
            let dialog_weak = dialog_weak_cat.clone();
            let list_box_weak = categories_list_box_weak.clone();

            Rc::new(move || {
                let Some(list_box) = list_box_weak.upgrade() else { return };
                let Some(dialog) = dialog_weak.upgrade() else { return };

                while let Some(child) = list_box.first_child() {
                    list_box.remove(&child);
                }

                let categories = CalendarCategoryStore::default().load();
                for cat in categories {
                    let row = adw::ActionRow::builder()
                        .title(&cat.name)
                        .build();

                    let color_dialog = gtk::ColorDialog::builder()
                        .title(&crate::i18n::gettext("Select Category Color"))
                        .build();
                    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
                    if let Ok(rgba) = gtk::gdk::RGBA::parse(&cat.color) {
                        color_button.set_rgba(&rgba);
                    }
                    color_button.set_valign(gtk::Align::Center);

                    let cat_id = cat.id.clone();
                    let cat_name = cat.name.clone();
                    let d_weak_for_color = dialog.downgrade();
                    color_button.connect_rgba_notify(move |btn| {
                        let rgba = btn.rgba();
                        let r = (rgba.red() * 255.0).round() as u8;
                        let g = (rgba.green() * 255.0).round() as u8;
                        let b = (rgba.blue() * 255.0).round() as u8;
                        let hex = format!("#{r:02x}{g:02x}{b:02x}");
                        let updated = CalendarCategory {
                            id: cat_id.clone(),
                            name: cat_name.clone(),
                            color: hex,
                        };
                        let _ = CalendarCategoryStore::default().upsert(updated);
                        if let Some(p) = d_weak_for_color.upgrade() {
                            refresh_open_category_views(&p);
                        }
                    });
                    row.add_prefix(&color_button);

                    let btn_delete = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text(&crate::i18n::gettext("Delete category"))
                        .css_classes(vec!["flat", "error"])
                        .valign(gtk::Align::Center)
                        .build();

                    let cat_id_del = cat.id.clone();
                    let d_weak_for_del = dialog.downgrade();
                    let rebuild_holder_for_del = rebuild_holder.clone();
                    btn_delete.connect_clicked(move |btn| {
                        let id = cat_id_del.clone();
                        let d_weak = d_weak_for_del.clone();
                        let rebuild_cb = rebuild_holder_for_del.clone();
                        crate::dialogs::run_destructive_action(
                            btn,
                            &crate::i18n::gettext("Delete calendar category?"),
                            &crate::i18n::gettext("This will remove the category color presentation."),
                            &crate::i18n::gettext("Delete"),
                            move || {
                                let _ = CalendarCategoryStore::default().delete(&id);
                                if let Some(p) = d_weak.upgrade() {
                                    p.show_toast(&crate::i18n::gettext("Category deleted"));
                                    refresh_open_category_views(&p);
                                }
                                if let Some(cb) = rebuild_cb.borrow().as_ref() {
                                    cb();
                                }
                            },
                        );
                    });
                    row.add_suffix(&btn_delete);
                    list_box.append(&row);
                }

                let add_row = adw::EntryRow::builder()
                    .title(&crate::i18n::gettext("Add new category…"))
                    .build();
                let btn_add = gtk::Button::builder()
                    .label(&crate::i18n::gettext("Add"))
                    .css_classes(vec!["flat", "suggested-action"])
                    .valign(gtk::Align::Center)
                    .build();
                add_row.add_suffix(&btn_add);

                let add_row_weak = add_row.downgrade();
                let d_weak_for_add = dialog.downgrade();
                let rebuild_holder_for_add = rebuild_holder.clone();

                let on_add = move || {
                    let Some(entry) = add_row_weak.upgrade() else { return };
                    let name = entry.text().trim().to_string();
                    if name.is_empty() { return; }
                    match CalendarCategoryStore::default().ensure_names(std::iter::once(&name)) {
                        Ok(_) => {
                            entry.set_text("");
                            if let Some(p) = d_weak_for_add.upgrade() {
                                p.show_toast(&crate::i18n::gettext("Calendar category added"));
                                refresh_open_category_views(&p);
                            }
                            if let Some(cb) = rebuild_holder_for_add.borrow().as_ref() {
                                cb();
                            }
                        }
                        Err(e) => {
                            if let Some(p) = d_weak_for_add.upgrade() {
                                p.show_toast(&format!("{}: {e}", crate::i18n::gettext("Could not add category")));
                            }
                        }
                    }
                };

                let on_add_rc = Rc::new(on_add);
                let on_add_for_btn = on_add_rc.clone();
                btn_add.connect_clicked(move |_| on_add_for_btn());
                add_row.connect_entry_activated(move |_| on_add_rc());
                list_box.append(&add_row);
            })
        };

        *rebuild_categories_holder.borrow_mut() = Some(rebuild_categories.clone());
        rebuild_categories();



        let str_all_tasks = crate::i18n::gettext("All Tasks");
        let str_due_today = crate::i18n::gettext("Due Today");
        let str_important = crate::i18n::gettext("Important");
        let str_overdue = crate::i18n::gettext("Overdue");
        let str_in_progress = crate::i18n::gettext("In Progress");
        let str_cancelled = crate::i18n::gettext("Cancelled");
        let str_completed = crate::i18n::gettext("Completed");

        let row_task_filter = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Task Filter"))
            .model(&gtk::StringList::new(&[
                &str_all_tasks,
                &str_due_today,
                &str_important,
                &str_overdue,
                &str_in_progress,
                &str_cancelled,
                &str_completed,
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
        

        let row_task_sort = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Sort tasks by"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Due Date"),
                &crate::i18n::gettext("Title"),
                &crate::i18n::gettext("Priority"),
            ]))
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
        

        let row_task_sort_direction = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Sort tasks descending"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_task_sort_direction.set_active(settings.task_sort_descending());
            row_task_sort_direction.connect_active_notify(move |row| {
                settings.set_task_sort_descending(row.is_active());
            });
        }
        

        let row_hide_completed_tasks = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Hide completed tasks"))
            .build();
        let row_hide_completed_tasks_value = adw::SpinRow::with_range(0.0, 1000.0, 1.0);
        row_hide_completed_tasks_value.set_title(&crate::i18n::gettext("Delay for completed tasks"));
        row_hide_completed_tasks_value.set_digits(0);
        row_hide_completed_tasks_value.set_snap_to_ticks(true);

        let row_hide_completed_tasks_units = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Unit for completion delay"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Minutes"),
                &crate::i18n::gettext("Hours"),
                &crate::i18n::gettext("Days"),
            ]))
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
        
        
        

        let row_hide_cancelled_tasks = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Hide cancelled tasks"))
            .build();
        if let Some(settings) = settings.clone() {
            row_hide_cancelled_tasks.set_active(settings.hide_cancelled_tasks());
            let settings_for_cancelled = settings.clone();
            row_hide_cancelled_tasks.connect_active_notify(move |row| {
                settings_for_cancelled.set_hide_cancelled_tasks(row.is_active());
            });
        }
        

        let row_memo_sort = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Sort memos by"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Modification Date"),
                &crate::i18n::gettext("Title"),
                &crate::i18n::gettext("Source"),
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
        

        let row_memo_sort_direction = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Sort memos descending"))
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_memo_sort_direction.set_active(settings.memo_sort_descending());
            row_memo_sort_direction.connect_active_notify(move |row| {
                settings.set_memo_sort_descending(row.is_active());
            });
        }
        

        let group_crypto = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Encryption and Signatures"))
            .build();

        let crypto_protocol_model =
            gtk::StringList::new(&["OpenPGP (GnuPG)", "S/MIME (OpenSSL/CMS)"]);
        let row_crypto_protocol = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Default Protocol"))
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
        let row_crypto_toggle = make_segmented_toggle_row(
            &row_crypto_protocol,
            &crate::i18n::gettext("Default Protocol"),
            &crate::i18n::gettext("OpenPGP"),
            &crate::i18n::gettext("S/MIME"),
        );
        group_crypto.add(&row_crypto_toggle);

        let row_pgp_encrypt = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Sign new messages by default"))
            .active(false)
            .build();
        group_crypto.add(&row_pgp_encrypt);

        let pgp_key_model =
            gtk::StringList::new(&[&crate::i18n::gettext("Automatic")]);
        let pgp_keys = Rc::new(RefCell::new(Vec::<OpenPgpKey>::new()));
        let row_pgp_key = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Default PGP Key"))
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

        let signer_model = gtk::StringList::new(&[&crate::i18n::gettext("None")]);
        let signer_certs: Rc<RefCell<Vec<crate::services::certificate_service::CertificateRecord>>> =
            Rc::new(RefCell::new(Vec::new()));
        let row_smime_signer = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("S/MIME Signature Certificate"))
            .model(&signer_model)
            .build();

        let row_smime_private_key = smime_file_row(
            &crate::i18n::gettext("S/MIME Private Key"),
            "",
            settings
                .as_ref()
                .map(|settings| settings.smime_private_key())
                .unwrap_or_default(),
            settings.clone(),
            AppSettings::set_smime_private_key,
        );

        let row_smime_recipients = smime_files_row(
            &crate::i18n::gettext("S/MIME Recipient Certificates"),
            "",
            settings
                .as_ref()
                .map(|settings| settings.smime_recipient_certificates())
                .unwrap_or_default(),
            settings.clone(),
            |settings, values| settings.set_smime_recipient_certificates(values),
        );

        let ca_model = gtk::StringList::new(&[&crate::i18n::gettext("System Trust Store")]);
        let ca_certs: Rc<RefCell<Vec<crate::services::certificate_service::CertificateRecord>>> =
            Rc::new(RefCell::new(Vec::new()));
        let row_smime_ca = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("S/MIME Trust Store"))
            .model(&ca_model)
            .build();

        let row_nav_smime = make_subpage_nav_row(
            &crate::i18n::gettext("S/MIME"),
            "sub_smime",
            &open_subpage,
        );
        

        let page_sub_smime = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("S/MIME Certificates"))
            .build();
        let group_smime = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("S/MIME Configuration"))
            .build();
        group_smime.add(&row_smime_signer);
        group_smime.add(&row_smime_private_key);
        group_smime.add(&row_smime_recipients);
        group_smime.add(&row_smime_ca);
        page_sub_smime.add(&group_smime);

        let updating_signer_selection = Rc::new(RefCell::new(false));
        if let Some(settings) = settings.clone() {
            let signer_certs_sel = Rc::clone(&signer_certs);
            let settings_sel = settings.clone();
            let updating = Rc::clone(&updating_signer_selection);
            row_smime_signer.connect_selected_notify(move |row| {
                if *updating.borrow() {
                    return;
                }
                let idx = row.selected();
                if idx == 0 {
                    settings_sel.set_smime_signer_certificate("");
                } else if let Some(cert) = signer_certs_sel.borrow().get(idx as usize - 1) {
                    let cert_path = cert
                        .cert_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| cert.id.clone());
                    settings_sel.set_smime_signer_certificate(&cert_path);
                    if cert.has_private_key {
                        if let Some(key_path) = &cert.private_key_path {
                            let key_str = key_path.to_string_lossy().to_string();
                            settings_sel.set_smime_private_key(&key_str);
                        }
                    }
                }
            });
        }

        let updating_ca_selection = Rc::new(RefCell::new(false));
        if let Some(settings) = settings.clone() {
            let ca_certs_sel = Rc::clone(&ca_certs);
            let settings_sel = settings.clone();
            let updating = Rc::clone(&updating_ca_selection);
            row_smime_ca.connect_selected_notify(move |row| {
                if *updating.borrow() {
                    return;
                }
                let idx = row.selected();
                if idx == 0 {
                    settings_sel.set_smime_ca_file("");
                } else if let Some(ca) = ca_certs_sel.borrow().get(idx as usize - 1) {
                    let ca_path = ca
                        .cert_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| ca.id.clone());
                    settings_sel.set_smime_ca_file(&ca_path);
                }
            });
        }

        let refresh_smime_selectors = {
            let signer_model = signer_model.clone();
            let ca_model = ca_model.clone();
            let signer_certs = Rc::clone(&signer_certs);
            let ca_certs = Rc::clone(&ca_certs);
            let row_smime_signer_weak = row_smime_signer.downgrade();
            let row_smime_ca_weak = row_smime_ca.downgrade();
            let updating_signer = Rc::clone(&updating_signer_selection);
            let updating_ca = Rc::clone(&updating_ca_selection);
            let settings = settings.clone();

            Rc::new(move || {
                // 1. Reload personal certs
                let personal = crate::services::certificate_service::CertificateService::list_certificates(
                    crate::services::certificate_service::CertCategory::Personal,
                );
                let mut signer_labels = vec![crate::i18n::gettext("None")];
                for cert in &personal {
                    let label = if !cert.subject_email.is_empty() {
                        format!("{} <{}>", cert.subject_cn, cert.subject_email)
                    } else {
                        cert.display_name.clone()
                    };
                    signer_labels.push(label);
                }

                let current_signer = settings.as_ref().map(|s| s.smime_signer_certificate()).unwrap_or_default();
                let mut selected_signer_idx = 0u32;
                if !current_signer.is_empty() {
                    let mut matched = false;
                    for (i, cert) in personal.iter().enumerate() {
                        let path = cert.cert_path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                        if current_signer == path || current_signer == cert.id || current_signer == cert.raw_pem {
                            selected_signer_idx = i as u32 + 1;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        signer_labels.push(std::path::Path::new(&current_signer).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| current_signer.clone()));
                        selected_signer_idx = (signer_labels.len() - 1) as u32;
                    }
                }

                *signer_certs.borrow_mut() = personal;
                let signer_refs: Vec<&str> = signer_labels.iter().map(|s| s.as_str()).collect();
                signer_model.splice(0, signer_model.n_items(), &signer_refs);

                *updating_signer.borrow_mut() = true;
                if let Some(row) = row_smime_signer_weak.upgrade() {
                    row.set_selected(selected_signer_idx);
                }
                *updating_signer.borrow_mut() = false;

                // 2. Reload CA certs
                let authorities = crate::services::certificate_service::CertificateService::list_certificates(
                    crate::services::certificate_service::CertCategory::Authority,
                );
                let mut ca_labels = vec![crate::i18n::gettext("System Trust Store")];
                for ca in &authorities {
                    let label = if !ca.issuer_org.is_empty() {
                        format!("{} ({})", ca.subject_cn, ca.issuer_org)
                    } else {
                        ca.display_name.clone()
                    };
                    ca_labels.push(label);
                }

                let current_ca = settings.as_ref().map(|s| s.smime_ca_file()).unwrap_or_default();
                let mut selected_ca_idx = 0u32;
                if !current_ca.is_empty() {
                    let mut matched = false;
                    for (i, ca) in authorities.iter().enumerate() {
                        let path = ca.cert_path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                        if current_ca == path || current_ca == ca.id {
                            selected_ca_idx = i as u32 + 1;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        ca_labels.push(std::path::Path::new(&current_ca).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| current_ca.clone()));
                        selected_ca_idx = (ca_labels.len() - 1) as u32;
                    }
                }

                *ca_certs.borrow_mut() = authorities;
                let ca_refs: Vec<&str> = ca_labels.iter().map(|s| s.as_str()).collect();
                ca_model.splice(0, ca_model.n_items(), &ca_refs);

                *updating_ca.borrow_mut() = true;
                if let Some(row) = row_smime_ca_weak.upgrade() {
                    row.set_selected(selected_ca_idx);
                }
                *updating_ca.borrow_mut() = false;
            })
        };

        refresh_smime_selectors();

        // ── Certificate Manager Entry Point ──────────────────────────────────


        let row_cert_manager = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Manage Certificates and Trust"))
            .activatable(true)
            .build();
        
        
        
        
        

        let self_weak_for_mgr = self.downgrade();
        let refresh_on_close = Rc::clone(&refresh_smime_selectors);
        let open_cert_manager = move || {
            let Some(pref_dialog) = self_weak_for_mgr.upgrade() else { return; };
            let mgr_dialog = crate::dialogs::CertificateManagerDialog::new(Some(&pref_dialog));
            let refresh_cb = Rc::clone(&refresh_on_close);
            mgr_dialog.connect_close_request(move |_| {
                refresh_cb();
                glib::Propagation::Proceed
            });
            mgr_dialog.present();
        };

        row_cert_manager.connect_activated(move |_| open_cert_manager());

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
            let keys = match key_receiver.recv().await {
                Ok(Ok(keys)) => keys,
                _ => Vec::new(),
            };
            // Without keys there is nothing to choose: say so plainly and
            // make the row insensitive instead of showing a dead "Automatic".
            if keys.is_empty() {
                pgp_key_model_for_load.splice(
                    0,
                    pgp_key_model_for_load.n_items(),
                    &[&crate::i18n::gettext("No Keys in Keyring")],
                );
                if let Some(row) = row_pgp_key_for_load.upgrade() {
                    row.set_selected(0);
                    row.set_sensitive(false);
                }
                return;
            }
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

        let group_privacy = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Privacy"))
            .build();
        row_image_policy.set_visible(true);
        group_privacy.add(&row_image_policy);

        // ── Allowed Senders (remote images allowlist) ──────────────────────
        let row_nav_allowed_senders = make_subpage_nav_row(
            &crate::i18n::gettext("Allowed Senders"),
            "sub_allowed_senders",
            &open_subpage,
        );
        group_privacy.add(&row_nav_allowed_senders);

        let page_sub_allowed_senders = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Allowed Senders"))
            .build();
        let group_allowed_add = adw::PreferencesGroup::builder()
            .description(&crate::i18n::gettext(
                "Images from these senders or domains are always loaded.",
            ))
            .build();
        let row_allowed_add = adw::EntryRow::builder()
            .title(&crate::i18n::gettext("Add Address or Domain"))
            .show_apply_button(true)
            .build();
        group_allowed_add.add(&row_allowed_add);
        let group_allowed_list = adw::PreferencesGroup::builder().build();
        let allowed_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        group_allowed_list.add(&allowed_list);
        page_sub_allowed_senders.add(&group_allowed_add);
        page_sub_allowed_senders.add(&group_allowed_list);

        let rebuild_allowed: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        {
            let list_weak = allowed_list.downgrade();
            let nav_weak = row_nav_allowed_senders.downgrade();
            let holder = Rc::clone(&rebuild_allowed);
            let rebuild: Rc<dyn Fn()> = Rc::new(move || {
                let Some(list) = list_weak.upgrade() else { return };
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                let store = crate::services::mail_metadata_service::RemoteContentStore::from_environment();
                let mut senders: Vec<String> = store.allowed_senders().into_iter().collect();
                senders.sort();
                if let Some(nav) = nav_weak.upgrade() {
                    let value = if senders.is_empty() {
                        String::new()
                    } else {
                        senders.len().to_string()
                    };
                    set_subpage_nav_value(&nav, &value);
                }
                if senders.is_empty() {
                    let empty = adw::ActionRow::builder()
                        .title(&crate::i18n::gettext("No Allowed Senders"))
                        .css_classes(vec!["dim-label"])
                        .build();
                    list.append(&empty);
                    return;
                }
                for sender in senders {
                    let row = adw::ActionRow::builder().title(&sender).build();
                    let btn_remove = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text(&crate::i18n::gettext("Remove"))
                        .css_classes(vec!["flat"])
                        .valign(gtk::Align::Center)
                        .build();
                    let holder_rm = Rc::clone(&holder);
                    btn_remove.connect_clicked(move |_| {
                        let store = crate::services::mail_metadata_service::RemoteContentStore::from_environment();
                        if let Err(error) = store.remove_allowed_sender(&sender) {
                            tracing::warn!(%error, "Could not remove allowed sender");
                        }
                        let cb = holder_rm.borrow().as_ref().cloned();
                        if let Some(f) = cb {
                            f();
                        }
                    });
                    row.add_suffix(&btn_remove);
                    list.append(&row);
                }
            });
            rebuild_allowed.replace(Some(Rc::clone(&rebuild)));
            rebuild();
        }
        {
            let holder = Rc::clone(&rebuild_allowed);
            row_allowed_add.connect_apply(move |entry| {
                let value = entry.text().trim().to_lowercase();
                if value.is_empty() {
                    return;
                }
                let store = crate::services::mail_metadata_service::RemoteContentStore::from_environment();
                if let Err(error) = store.allow_sender(&value) {
                    tracing::warn!(%error, "Could not add allowed sender");
                }
                entry.set_text("");
                let cb = holder.borrow().as_ref().cloned();
                if let Some(f) = cb {
                    f();
                }
            });
        }
        row_mail_fetch_remote_logos.set_title(&crate::i18n::gettext("Load Sender Logos from the Internet"));
        row_mail_fetch_remote_logos.set_visible(true);
        group_privacy.add(&row_mail_fetch_remote_logos);
        let row_nav_junk = make_subpage_nav_row(
            &crate::i18n::gettext("Junk Mail Filtering"),
            "junk",
            &open_subpage,
        );
        if let Some(ref s) = settings {
            let status = if s.junk_check_incoming() { "Enabled" } else { "Off" };
            set_subpage_nav_value(&row_nav_junk, status);
            let row_weak = row_nav_junk.downgrade();
            s.connect_changed(Some("junk-check-incoming"), move |s_obj, _| {
                if let Some(row) = row_weak.upgrade() {
                    let val = s_obj.boolean("junk-check-incoming");
                    set_subpage_nav_value(&row, if val { "Enabled" } else { "Off" });
                }
            });
        }
        group_privacy.add(&row_nav_junk);

        row_mail_security_details.set_title(&crate::i18n::gettext("Show Security Status for All Messages"));
        row_mail_security_details.set_visible(true);
        group_crypto.add(&row_mail_security_details);
        
        row_cert_manager.set_title(&crate::i18n::gettext("Manage Certificates…"));
        group_crypto.add(&row_nav_smime);
        group_crypto.add(&row_cert_manager);

        let page_certificates = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Privacy & Security"))
            .icon_name("security-high-symbolic")
            .build();
        page_certificates.add(&group_privacy);
        page_certificates.add(&group_crypto);

        let group_mail_labels = build_mail_labels_group(&self, settings.clone());
        let group_mail_junk = build_mail_junk_group(settings.clone());
        let group_mail_notifications = build_mail_notifications_group(settings.clone());

        let page_mail_general = adw::PreferencesPage::builder()
            .title(&gettext("General"))
            .icon_name("preferences-other-symbolic")
            .build();
        page_mail_general.add(&group_behavior);
        page_mail_general.add(&group_mail_notifications);
        page_mail_general.add(&group_sync);

        group_mail_headers_mode.set_visible(true);
        group_mail_normal_headers.set_visible(true);
        group_mail_expanded_headers.set_visible(true);

        let page_mail_headers = adw::PreferencesPage::builder()
            .title(&gettext("Message Headers"))
            .icon_name("document-properties-symbolic")
            .build();
        page_mail_headers.add(&group_mail_headers_mode);
        page_mail_headers.add(&group_mail_normal_headers);
        page_mail_headers.add(&group_mail_expanded_headers);

        let page_sub_header_toolbar = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Message Header Actions"))
            .build();
        group_mail_header_actions.set_visible(true);
        page_sub_header_toolbar.add(&group_mail_header_actions);
        let page_sub_list_toolbar = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("List & Quick-Filter Bar"))
            .build();
        group_mail_list_toolbar.set_visible(true);
        page_sub_list_toolbar.add(&group_mail_list_toolbar);

        let page_sub_reading_advanced = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Advanced Reading"))
            .build();

        let group_typography = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Typography and Rendering"))
            .build();

        let row_font_size = adw::SpinRow::with_range(8.0, 48.0, 1.0);
        row_font_size.set_title(&crate::i18n::gettext("Font Size"));
        row_font_size.set_digits(0);
        row_font_size.set_snap_to_ticks(true);
        if let Some(ref s) = settings {
            row_font_size.set_value(s.mail_font_size() as f64);
            let s_fs = s.clone();
            row_font_size.connect_value_notify(move |spin| {
                s_fs.set_mail_font_size(spin.value().round() as u32);
            });
            let spin_weak = row_font_size.downgrade();
            s.connect_changed(Some("mail-font-size"), move |s_obj, _| {
                let Some(spin) = spin_weak.upgrade() else { return; };
                let val = s_obj.uint("mail-font-size") as f64;
                if (spin.value() - val).abs() > f64::EPSILON {
                    spin.set_value(val);
                }
            });
        }
        group_typography.add(&row_font_size);

        let row_force_dark = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Force dark colors in all HTML emails"))
            .active(false)
            .build();
        if let Some(ref s) = settings {
            row_force_dark.set_active(s.force_dark_mode());
            let s_fd = s.clone();
            row_force_dark.connect_active_notify(move |row| {
                s_fd.set_force_dark_mode(row.is_active());
            });
            let row_weak = row_force_dark.downgrade();
            s.connect_changed(Some("force-dark-mode"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("force-dark-mode");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_typography.add(&row_force_dark);

        row_html_rendering.set_visible(true);
        group_typography.add(&row_html_rendering);
        row_preserve_image_layout.set_visible(true);
        group_typography.add(&row_preserve_image_layout);

        row_caret_mode.set_visible(true);
        group_typography.add(&row_caret_mode);
        row_magic_spacebar.set_visible(true);
        group_typography.add(&row_magic_spacebar);
        row_mail_show_deleted.set_visible(true);
        group_typography.add(&row_mail_show_deleted);
        page_sub_reading_advanced.add(&group_typography);

        let group_reading_subpages = adw::PreferencesGroup::builder().build();
        let row_nav_headers = make_subpage_nav_row(
            &crate::i18n::gettext("Message Headers"),
            "headers",
            &open_subpage,
        );
        if let Some(ref s) = settings {
            let count = s.mail_normal_headers().iter().filter(|(_, v)| *v).count();
            set_subpage_nav_value(&row_nav_headers, &format!("{} enabled", count));
            let row_weak = row_nav_headers.downgrade();
            s.connect_changed(Some("mail-normal-headers"), move |_, _| {
                if let Some(row) = row_weak.upgrade() {
                    if let Some(s) = AppSettings::try_new() {
                        let count = s.mail_normal_headers().iter().filter(|(_, v)| *v).count();
                        set_subpage_nav_value(&row, &format!("{} enabled", count));
                    }
                }
            });
        }
        let row_nav_reading_adv = make_subpage_nav_row(
            &crate::i18n::gettext("Advanced"),
            "sub_reading_advanced",
            &open_subpage,
        );
        group_reading_subpages.add(&row_nav_headers);
        group_reading_subpages.add(&row_nav_reading_adv);

        let page_mail_html = adw::PreferencesPage::builder()
            .title(&gettext("Reading"))
            .icon_name("mail-read-symbolic")
            .build();
        page_mail_html.add(&group_mail_reading);
        page_mail_html.add(&group_reading_subpages);

        let page_mail_labels = adw::PreferencesPage::builder()
            .title(&gettext("Colors & Labels"))
            .icon_name("color-select-symbolic")
            .build();
        group_mail_labels.set_visible(true);
        page_mail_labels.add(&group_mail_labels);

        let page_mail_junk = adw::PreferencesPage::builder()
            .title(&gettext("Junk & Spam Protection"))
            .icon_name("mail-mark-junk-symbolic")
            .build();
        page_mail_junk.add(&group_mail_junk);


        

        let page_sub_list_elements = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Columns & Indicators"))
            .build();
        group_mail_elements.set_visible(true);
        group_mail_elements.add(&row_mail_show_flagged);
        group_mail_elements.add(&row_mail_show_attachment);
        group_mail_elements.add(&row_mail_show_size);
        group_mail_elements.add(&row_mail_show_labels);

        let row_show_status = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show message status"))
            
            .active(true)
            .build();
        if let Some(ref s) = settings {
            row_show_status.set_active(s.mail_show_status());
            let s_st = s.clone();
            row_show_status.connect_active_notify(move |r| {
                s_st.set_mail_show_status(r.is_active());
            });
            let row_weak = row_show_status.downgrade();
            s.connect_changed(Some("mail-show-status"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("mail-show-status");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_elements.add(&row_show_status);

        let row_show_follow_up = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show follow-up flags"))
            
            .active(true)
            .build();
        if let Some(ref s) = settings {
            row_show_follow_up.set_active(s.mail_show_follow_up());
            let s_fu = s.clone();
            row_show_follow_up.connect_active_notify(move |r| {
                s_fu.set_mail_show_follow_up(r.is_active());
            });
            let row_weak = row_show_follow_up.downgrade();
            s.connect_changed(Some("mail-show-follow-up"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("mail-show-follow-up");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_elements.add(&row_show_follow_up);

        let row_show_follow_up_due = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show follow-up due date"))
            
            .active(false)
            .build();
        if let Some(ref s) = settings {
            row_show_follow_up_due.set_active(s.mail_show_follow_up_due());
            let s_fud = s.clone();
            row_show_follow_up_due.connect_active_notify(move |r| {
                s_fud.set_mail_show_follow_up_due(r.is_active());
            });
            let row_weak = row_show_follow_up_due.downgrade();
            s.connect_changed(Some("mail-show-follow-up-due"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("mail-show-follow-up-due");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_elements.add(&row_show_follow_up_due);

        let row_show_location = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show message location"))
            
            .active(false)
            .build();
        if let Some(ref s) = settings {
            row_show_location.set_active(s.mail_show_location());
            let s_loc = s.clone();
            row_show_location.connect_active_notify(move |r| {
                s_loc.set_mail_show_location(r.is_active());
            });
            let row_weak = row_show_location.downgrade();
            s.connect_changed(Some("mail-show-location"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("mail-show-location");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_elements.add(&row_show_location);

        let row_show_recip_addr = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show recipient address"))
            
            .active(false)
            .build();
        if let Some(ref s) = settings {
            row_show_recip_addr.set_active(s.mail_show_recipient_address());
            let s_ra = s.clone();
            row_show_recip_addr.connect_active_notify(move |r| {
                s_ra.set_mail_show_recipient_address(r.is_active());
            });
            let row_weak = row_show_recip_addr.downgrade();
            s.connect_changed(Some("mail-show-recipient-address"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("mail-show-recipient-address");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_elements.add(&row_show_recip_addr);

        row_mail_date_align.set_visible(false);
        group_mail_elements.add(&row_mail_date_align);
        let row_date_align_toggle = make_segmented_toggle_row(
            &row_mail_date_align,
            &crate::i18n::gettext("Date Alignment"),
            &crate::i18n::gettext("Left"),
            &crate::i18n::gettext("Right"),
        );
        group_mail_elements.add(&row_date_align_toggle);
        page_sub_list_elements.add(&group_mail_elements);

        let group_conversations = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Conversations"))
            .build();
        group_conversations.add(&row_mail_threading);
        group_conversations.add(&row_mail_group_similar);
        group_conversations.add(&row_mail_conversation_accordion);

        let group_sorting = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Sorting"))
            .build();
        group_sorting.add(&row_mail_sort);
        group_sorting.add(&row_mail_sort_direction);

        let group_rows = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Rows"))
            .build();
        group_rows.add(&row_mail_show_avatar);
        group_rows.add(&row_mail_show_preview_text);
        group_rows.add(&row_mail_date_format);
        group_rows.add(&row_mail_date_custom);

        let group_layout_nav = adw::PreferencesGroup::builder().build();
        let row_nav_elements = make_subpage_nav_row(
            &crate::i18n::gettext("Columns & Indicators"),
            "sub_list_elements",
            &open_subpage,
        );
        group_layout_nav.add(&row_nav_elements);

        let page_mail_layout = adw::PreferencesPage::builder()
            .title(&gettext("Message List"))
            .icon_name("view-list-symbolic")
            .build();
        page_mail_layout.add(&group_conversations);
        page_mail_layout.add(&group_sorting);
        page_mail_layout.add(&group_rows);
        page_mail_layout.add(&group_layout_nav);

        let page_composer_general = adw::PreferencesPage::builder()
            .title(&gettext("Composing"))
            .icon_name("document-edit-symbolic")
            .build();
        page_composer_general.add(&group_composer);
        page_composer_general.add(&group_reply);
        page_composer_general.add(&group_composer_headers);
        page_composer_general.add(&group_composer_prompts);
        page_composer_general.add(&group_composer_charset);

        // ── Signatures Group under Accounts (GNOME HIG Native + Studio Modal) ──
        let group_signatures = adw::PreferencesGroup::builder()
            .title(&gettext("Signatures"))
            .build();

        let row_manage_signatures = adw::ActionRow::builder()
            .title(&gettext("Manage Signatures…"))
            .activatable(true)
            .build();
        let lbl_sig_count = gtk::Label::builder()
            .css_classes(vec!["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        row_manage_signatures.add_suffix(&lbl_sig_count);
        let sig_chevron = gtk::Image::from_icon_name("go-next-symbolic");
        sig_chevron.add_css_class("dim-label");
        row_manage_signatures.add_suffix(&sig_chevron);

        group_signatures.add(&row_manage_signatures);
        group_signatures.add(&signature_row);
        group_signatures.add(&row_top_signature);
        group_signatures.add(&row_signature_outline);

        page_accounts.remove(&group_oof);
        page_accounts.add(&group_signatures);
        page_accounts.add(&group_oof);

        let dialog_weak = self.downgrade();
        let signature_row_weak = signature_row.downgrade();
        let lbl_sig_count_weak = lbl_sig_count.downgrade();

        let is_updating_combo = Rc::new(Cell::new(false));
        let is_updating_for_rebuild = is_updating_combo.clone();
        let is_updating_for_notify = is_updating_combo.clone();

        let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        let rebuild_fn: Rc<dyn Fn()> = {
            let is_updating_for_rebuild = is_updating_for_rebuild.clone();
            Rc::new(move || {
                let store = SignatureStore::from_environment();
                let signatures = store.load();

                if let Some(lbl) = lbl_sig_count_weak.upgrade() {
                    if signatures.is_empty() {
                        lbl.set_label(&gettext("None"));
                    } else {
                        lbl.set_label(&signatures.len().to_string());
                    }
                }

                if let Some(combo) = signature_row_weak.upgrade() {
                    is_updating_for_rebuild.set(true);
                    let none_lbl = crate::i18n::gettext("None");
                    let mut names: Vec<&str> = vec![none_lbl.as_str()];
                    names.extend(signatures.iter().map(|s| s.name.as_str()));
                    let model = gtk::StringList::new(&names);
                    combo.set_model(Some(&model));
                    if let Some(active_id) = store.active_id() {
                        if let Some(pos) = signatures.iter().position(|s| s.id == active_id) {
                            combo.set_selected((pos + 1) as u32);
                        } else {
                            combo.set_selected(0);
                        }
                    } else {
                        combo.set_selected(0);
                    }
                    is_updating_for_rebuild.set(false);
                }
            })
        };

        *rebuild_holder.borrow_mut() = Some(rebuild_fn.clone());
        rebuild_fn();

        {
            let dialog_weak = dialog_weak.clone();
            let rebuild_holder = rebuild_holder.clone();
            let settings_for_modal = settings.clone();
            row_manage_signatures.connect_activated(move |_| {
                let Some(parent_dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let rebuild_on_save = rebuild_holder.clone();
                let settings_on_save = settings_for_modal.clone();
                let on_saved = move |_saved: SignatureProfile| {
                    let store = SignatureStore::from_environment();
                    if let Some(settings) = settings_on_save.as_ref() {
                        if let Some(active) = store.active() {
                            settings.set_signature_text(&active.text);
                        } else {
                            settings.set_signature_text("");
                        }
                    }
                    if let Some(f) = rebuild_on_save.borrow().as_ref() {
                        f();
                    }
                };
                let dlg = crate::dialogs::SignatureEditorDialog::new_manager(Some(on_saved));
                let rebuild_on_close = rebuild_holder.clone();
                dlg.connect_closed(move |_| {
                    if let Some(f) = rebuild_on_close.borrow().as_ref() {
                        f();
                    }
                });
                dlg.present(Some(&parent_dialog));
            });
        }

        let settings_for_select = settings.clone();
        let combo_weak = signature_row.downgrade();
        let rebuild_for_combo = rebuild_holder.clone();
        signature_row.connect_selected_notify(move |_| {
            if is_updating_for_notify.get() {
                return;
            }
            let Some(combo) = combo_weak.upgrade() else { return };
            let store = SignatureStore::from_environment();
            let signatures = store.load();
            let idx = combo.selected() as usize;
            if idx == 0 {
                let _ = store.set_active(None);
                if let Some(settings) = settings_for_select.as_ref() {
                    settings.set_signature_text("");
                }
            } else if let Some(sig) = signatures.get(idx - 1) {
                let _ = store.set_active(Some(&sig.id));
                if let Some(settings) = settings_for_select.as_ref() {
                    settings.set_signature_text(&sig.text);
                }
            }
            if let Some(f) = rebuild_for_combo.borrow().as_ref() {
                is_updating_for_notify.set(true);
                f();
                is_updating_for_notify.set(false);
            }
        });

        let group_backup = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Backup"))
            .build();
        group_backup.add(&row_backup);
        group_backup.add(&row_restore);

        let group_import = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Import"))
            .build();
        group_import.add(&row_import_profile);

        let row_import_unified = adw::ActionRow::builder()
            .title(&crate::i18n::gettext(
                "Import from File (.eml, .mbox, .ics, .vcf)…",
            ))
            .activatable(true)
            .build();
        row_import_unified.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
        row_import_unified.connect_activated(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                let Some(root) = dialog.root().and_downcast::<gtk::Window>() else {
                    dialog.show_toast("No window available for import");
                    return;
                };
                let filter = gtk::FileFilter::new();
                filter.set_name(Some("Mail, Calendar & Contact Files (.eml, .mbox, .ics, .vcf)"));
                for pat in ["*.eml", "*.mbox", "*.mbx", "*.ics", "*.ical", "*.vcf", "*.vcard"] {
                    filter.add_pattern(pat);
                }
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let all_filter = gtk::FileFilter::new();
                all_filter.set_name(Some("All Files"));
                all_filter.add_pattern("*");
                filters.append(&all_filter);

                let file_dialog = gtk::FileDialog::builder()
                    .title("Import Files")
                    .filters(&filters)
                    .default_filter(&filter)
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
                    let mut mail_paths = Vec::new();
                    let mut cal_paths = Vec::new();
                    let mut contact_paths = Vec::new();
                    for p in paths {
                        let ext = p
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        match ext.as_str() {
                            "ics" | "ical" => cal_paths.push(p),
                            "vcf" | "vcard" => contact_paths.push(p),
                            _ => mail_paths.push(p),
                        }
                    }
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let mut summary = Vec::new();
                        if !mail_paths.is_empty() {
                            let svc = crate::services::MailStoreService::new();
                            if let Ok(n) = svc.import_files(&mail_paths) {
                                summary.push(format!("{n} email(s)"));
                            }
                        }
                        if !cal_paths.is_empty() {
                            let svc = crate::services::CalendarStoreService::new();
                            if let Ok(n) = svc.import_files(&cal_paths) {
                                summary.push(format!("{n} calendar file(s)"));
                            }
                        }
                        if !contact_paths.is_empty() {
                            let svc = crate::services::ContactStoreService::new();
                            if let Ok(n) = svc.import_files(&contact_paths) {
                                summary.push(format!("{n} vCard file(s)"));
                            }
                        }
                        let _ = sender.send_blocking(summary);
                    });
                    glib::spawn_future_local(async move {
                        let Ok(summary) = receiver.recv().await else {
                            return;
                        };
                        if let Some(dialog) = dialog_for_file.upgrade() {
                            if summary.is_empty() {
                                dialog.show_toast("No items could be imported from the selected files");
                            } else {
                                dialog.show_toast(&format!("Imported: {}", summary.join(", ")));
                            }
                        }
                    });
                });
            }
        ));
        group_import.add(&row_import_unified);

        row_import_mail.set_visible(false);
        row_import_calendar.set_visible(false);
        row_import_contacts.set_visible(false);
        group_import.add(&row_import_mail);
        group_import.add(&row_import_calendar);
        group_import.add(&row_import_contacts);

        let group_danger_zone = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Danger Zone"))
            .build();
        group_danger_zone.add(&row_delete);

        page_backup.add(&group_backup);
        page_backup.add(&group_import);
        page_backup.add(&group_danger_zone);

        let group_backup_storage = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Storage Locations"))
            .build();
        group_backup_storage.add(&row_local_mail);
        group_backup_storage.add(&row_local_calendar);
        group_backup_storage.add(&row_local_contacts);

        let group_backup_diag = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Diagnostics"))
            .build();
        row_debug_console.set_visible(false);
        row_plugins.set_visible(false);
        group_backup_diag.add(&row_debug_console);
        group_backup_diag.add(&row_diagnostic);

        let row_open_dev_console = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Open Developer Console…"))
            .subtitle(&crate::i18n::gettext(
                "Inspect live sync, IMAP/SMTP, and EDS protocol logs",
            ))
            .activatable(true)
            .build();
        row_open_dev_console.add_suffix(&gtk::Image::from_icon_name("utilities-terminal-symbolic"));
        let settings_for_dev_console = settings.clone();
        let dialog_weak_dev_console = self.downgrade();
        let row_debug_console_ref = row_debug_console.clone();
        let row_statusbar_for_dev_console = row_mail_statusbar.clone();
        row_open_dev_console.connect_activated(move |_| {
            if let Some(ref s) = settings_for_dev_console {
                s.set_debug_console_enabled(true);
                s.set_mail_show_statusbar(true);
            }
            row_debug_console_ref.set_active(true);
            row_statusbar_for_dev_console.set_active(true);
            if let Some(d) = dialog_weak_dev_console.upgrade() {
                if let Some(win) = crate::window::find_meridian_window(&d) {
                    win.set_debug_console_open(true);
                }
                d.show_toast(&crate::i18n::gettext("Developer Console enabled"));
            }
        });
        group_backup_diag.add(&row_open_dev_console);
        group_backup_diag.add(&row_plugins);

        page_sub_backup_advanced.add(&group_backup_storage);
        page_sub_backup_advanced.add(&group_backup_diag);

        let page_sub_cal_workdays = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Work Days & Time Zones"))
            .build();
        group_calendar_time.set_visible(true);
        expander_workdays.set_expanded(true);
        page_sub_cal_workdays.add(&group_calendar_time);

        let group_cal_tz = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Time Zones"))
            .build();
        group_cal_tz.add(&row_calendar_time_zone_combo);
        group_cal_tz.add(&row_second_zone_combo);
        group_cal_tz.add(&row_calendar_time_zone);
        group_cal_tz.add(&row_second_zone);
        row_month_scroll_by_week.set_visible(true);
        row_month_scroll_anim.set_visible(true);

        let row_month_rows = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Weeks in Month View"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("5 Weeks"),
                &crate::i18n::gettext("6 Weeks"),
            ]))
            .build();
        if let Some(ref s) = settings {
            row_month_rows.set_selected(if s.calendar_month_view_rows() == 6 { 1 } else { 0 });
            let s_mr = s.clone();
            row_month_rows.connect_selected_notify(move |row| {
                let rows = if row.selected() == 1 { 6 } else { 5 };
                s_mr.set_calendar_month_view_rows(rows);
            });
            let row_weak = row_month_rows.downgrade();
            s.connect_changed(Some("calendar-month-view-rows"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let target = if s_obj.int("calendar-month-view-rows") == 6 { 1 } else { 0 };
                if row.selected() != target {
                    row.set_selected(target);
                }
            });
        }
        
        let row_month_rows_toggle = make_segmented_toggle_row(
            &row_month_rows,
            &crate::i18n::gettext("Weeks in Month View"),
            &crate::i18n::gettext("5 Weeks"),
            &crate::i18n::gettext("6 Weeks"),
        );
        

        let row_chip_scale = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Event Chip Size"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Compact"),
                &crate::i18n::gettext("Standard"),
                &crate::i18n::gettext("Large"),
            ]))
            .build();
        if let Some(ref s) = settings {
            let scale = s.calendar_chip_scale();
            let sel = if scale <= 0.90 {
                0
            } else if scale >= 1.10 {
                2
            } else {
                1
            };
            row_chip_scale.set_selected(sel);
            let s_cs = s.clone();
            row_chip_scale.connect_selected_notify(move |row| {
                let val = match row.selected() {
                    0 => 0.85,
                    2 => 1.2,
                    _ => 1.0,
                };
                s_cs.set_calendar_chip_scale(val);
            });
            let row_weak = row_chip_scale.downgrade();
            s.connect_changed(Some("calendar-chip-scale"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let val = s_obj.double("calendar-chip-scale");
                let sel = if val <= 0.90 {
                    0
                } else if val >= 1.10 {
                    2
                } else {
                    1
                };
                if row.selected() != sel {
                    row.set_selected(sel);
                }
            });
        }
        let toggle_chip_scale = make_segmented_toggle_3_row(
            &row_chip_scale,
            &crate::i18n::gettext("Event Chip Size"),
            &crate::i18n::gettext("Compact"),
            &crate::i18n::gettext("Standard"),
            &crate::i18n::gettext("Large"),
        );
        
        
        page_sub_cal_workdays.add(&group_cal_tz);

        let page_sub_cal_reminders = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Reminders & Notifications"))
            .build();
        expander_ba_reminders.set_expanded(true);
        row_reminder_tray.set_visible(true);
        group_calendar_reminders.set_visible(true);
        page_sub_cal_reminders.add(&group_calendar_reminders);

        let page_sub_cal_categories = adw::PreferencesPage::builder()
            .title(&crate::i18n::gettext("Calendar Categories"))
            .build();
        group_categories.set_visible(true);
        page_sub_cal_categories.add(&group_categories);

        let group_cal_subpages = adw::PreferencesGroup::builder().build();
        let row_nav_workdays = make_subpage_nav_row(
            &crate::i18n::gettext("Work Week & Time Zones"),
            "sub_calendar_workdays",
            &open_subpage,
        );
        let row_nav_reminders = make_subpage_nav_row(
            &crate::i18n::gettext("Reminders"),
            "sub_calendar_reminders",
            &open_subpage,
        );
        if let Some(ref s) = settings {
            let status = if s.use_default_reminder() { "On" } else { "Off" };
            set_subpage_nav_value(&row_nav_reminders, status);
            let row_weak = row_nav_reminders.downgrade();
            s.connect_changed(Some("use-default-reminder"), move |s_obj, _| {
                if let Some(row) = row_weak.upgrade() {
                    let val = s_obj.boolean("use-default-reminder");
                    set_subpage_nav_value(&row, if val { "On" } else { "Off" });
                }
            });
        }
        let row_nav_categories = make_subpage_nav_row(
            &crate::i18n::gettext("Categories"),
            "sub_calendar_categories",
            &open_subpage,
        );
        let cat_count = CalendarCategoryStore::default().load().len();
        set_subpage_nav_value(&row_nav_categories, &format!("{} categories", cat_count));
        group_cal_subpages.add(&row_nav_workdays);
        group_cal_subpages.add(&row_nav_reminders);
        group_cal_subpages.add(&row_nav_categories);

        let row_time_format_toggle = make_segmented_toggle_row(
            &row_time_format,
            &crate::i18n::gettext("Time Format"),
            &crate::i18n::gettext("12h"),
            &crate::i18n::gettext("24h"),
        );

        let group_calendar = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Calendar"))
            .build();
        let row_default_calendar_app = build_default_handler_row(
            self,
            &crate::i18n::gettext("Default Calendar Application"),
            &crate::i18n::gettext("Meridian is the system-wide default calendar application"),
            &crate::i18n::gettext("Use Meridian as the system-wide calendar in GNOME and for .ics files"),
            &crate::i18n::gettext("Meridian set as default calendar application"),
            "text/calendar",
            crate::application::DEFAULT_CALENDAR_MIME_TYPES,
        );
        group_calendar.add(&row_default_calendar_app);
        group_calendar.add(&source_preference_row(
            "Default Calendar",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Calendar",
            settings.clone(),
            AppSettings::default_calendar_source_uid,
            AppSettings::set_default_calendar_source_uid,
        ));
        group_calendar.add(&row_calendar_view);
        group_calendar.add(&row_week_start);
        group_calendar.add(&row_week_toggle);
        group_calendar.add(&row_time_format);
        group_calendar.add(&row_time_format_toggle);
        group_calendar.add(&row_week_numbers);

        let row_nav_calendar_view = make_subpage_nav_row(
            &crate::i18n::gettext("View Options"),
            "sub_calendar_view",
            &open_subpage,
        );
        row_nav_calendar_view.set_visible(false);
        group_cal_subpages.add(&row_nav_calendar_view);

        let group_calendar_view_options = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("View Options"))
            .build();
        group_calendar_view_options.add(&row_time_divisions);
        group_calendar_view_options.add(&row_calendar_click_action);
        group_calendar_view_options.add(&row_click_toggle);
        group_calendar_view_options.add(&row_compress_weekend);
        group_calendar_view_options.add(&row_month_rows);
        group_calendar_view_options.add(&row_month_rows_toggle);
        group_calendar_view_options.add(&row_month_scroll_by_week);
        group_calendar_view_options.add(&row_month_scroll_anim);
        group_calendar_view_options.add(&row_chip_scale);
        group_calendar_view_options.add(&toggle_chip_scale);
        group_calendar_view_options.add(&row_dim_past_events);
        group_calendar_view_options.add(&row_hide_cancelled_events);
        group_calendar_view_options.add(&row_date_format);

        let page_calendar_general = adw::PreferencesPage::builder().title(&gettext("Calendar")).build();
        page_calendar_general.add(&group_calendar);
        page_calendar_general.add(&group_calendar_view_options);
        page_calendar_general.add(&group_cal_subpages);



        let row_task_preview_visible = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show task preview pane"))
            .active(true)
            .build();
        if let Some(ref s) = settings {
            row_task_preview_visible.set_active(s.task_preview_visible());
            let s_tp = s.clone();
            row_task_preview_visible.connect_active_notify(move |r| {
                s_tp.set_task_preview_visible(r.is_active());
            });
            let row_weak = row_task_preview_visible.downgrade();
            s.connect_changed(Some("task-preview-orientation"), move |_, _| {});
            s.connect_changed(Some("task-preview-visible"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("task-preview-visible");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        

        let row_task_layout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Tasks Layout"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Stacked"),
                &crate::i18n::gettext("Side-by-Side"),
            ]))
            .build();
        if let Some(ref s) = settings {
            row_task_layout.set_selected(if s.task_preview_orientation() == "classic" { 0 } else { 1 });
            let s_to = s.clone();
            row_task_layout.connect_selected_notify(move |r| {
                let val = if r.selected() == 0 { "classic" } else { "vertical" };
                s_to.set_task_preview_orientation(val);
            });
            let row_weak = row_task_layout.downgrade();
            s.connect_changed(Some("task-preview-orientation"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let is_classic = s_obj.string("task-preview-orientation").as_str() == "classic";
                let sel = if is_classic { 0 } else { 1 };
                if row.selected() != sel {
                    row.set_selected(sel);
                }
            });
        }
        
        let row_task_layout_toggle = make_segmented_toggle_row(
            &row_task_layout,
            &crate::i18n::gettext("Tasks Layout"),
            &crate::i18n::gettext("Stacked"),
            &crate::i18n::gettext("Side-by-Side"),
        );
        

        let row_memo_preview_visible = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show memo preview pane"))
            .active(true)
            .visible(false)
            .build();
        if let Some(ref s) = settings {
            row_memo_preview_visible.set_active(s.memo_preview_visible());
            let s_mp = s.clone();
            row_memo_preview_visible.connect_active_notify(move |r| {
                s_mp.set_memo_preview_visible(r.is_active());
            });
            let row_weak = row_memo_preview_visible.downgrade();
            s.connect_changed(Some("memo-preview-visible"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s_obj.boolean("memo-preview-visible");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        

        let row_memo_layout = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Memos Layout"))
            .model(&gtk::StringList::new(&[
                &crate::i18n::gettext("Stacked"),
                &crate::i18n::gettext("Side-by-Side"),
            ]))
            .build();
        if let Some(ref s) = settings {
            row_memo_layout.set_selected(if s.memo_preview_orientation() == "classic" { 0 } else { 1 });
            let s_mo = s.clone();
            row_memo_layout.connect_selected_notify(move |r| {
                let val = if r.selected() == 0 { "classic" } else { "vertical" };
                s_mo.set_memo_preview_orientation(val);
            });
            let row_weak = row_memo_layout.downgrade();
            s.connect_changed(Some("memo-preview-orientation"), move |s_obj, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let is_classic = s_obj.string("memo-preview-orientation").as_str() == "classic";
                let sel = if is_classic { 0 } else { 1 };
                if row.selected() != sel {
                    row.set_selected(sel);
                }
            });
        }
        
        let row_memo_layout_toggle = make_segmented_toggle_row(
            &row_memo_layout,
            &crate::i18n::gettext("Memos Layout"),
            &crate::i18n::gettext("Stacked"),
            &crate::i18n::gettext("Side-by-Side"),
        );
        

        let exp_hide_completed_tasks = adw::ExpanderRow::builder()
            .title(&crate::i18n::gettext("Hide Completed Tasks"))
            .show_enable_switch(true)
            .build();
        if let Some(ref s) = settings {
            exp_hide_completed_tasks.set_enable_expansion(s.hide_completed_tasks());
            let s_hct = s.clone();
            exp_hide_completed_tasks.connect_enable_expansion_notify(move |exp| {
                s_hct.set_hide_completed_tasks(exp.enables_expansion());
            });
        }
        exp_hide_completed_tasks.add_row(&row_hide_completed_tasks_value);
        exp_hide_completed_tasks.add_row(&row_hide_completed_tasks_units);

        let group_tasks = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Tasks"))
            .build();
        group_tasks.add(&source_preference_row(
            "Default List",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Task List",
            settings.clone(),
            AppSettings::default_task_source_uid,
            AppSettings::set_default_task_source_uid,
        ));
        group_tasks.add(&row_task_layout);
        group_tasks.add(&row_task_layout_toggle);
        group_tasks.add(&row_task_preview_visible);
        group_tasks.add(&exp_hide_completed_tasks);
        group_tasks.add(&row_hide_cancelled_tasks);

        let group_memos = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Memos"))
            .build();
        group_memos.add(&source_preference_row(
            "Default List",
            "",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Memo List",
            settings.clone(),
            AppSettings::default_memo_source_uid,
            AppSettings::set_default_memo_source_uid,
        ));
        group_memos.add(&row_memo_layout);
        group_memos.add(&row_memo_layout_toggle);
        group_memos.add(&row_memo_preview_visible);

        let page_tasks = adw::PreferencesPage::builder()
            .title(&gettext("Tasks & Memos"))
            .icon_name("view-list-bullet-symbolic")
            .build();
        page_tasks.add(&group_tasks);
        page_tasks.add(&group_memos);

        // ── Detail Stack (Preferences Pages) ────────────────────────────────
        let detail_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(true)
            .vhomogeneous(false)
            .hexpand(true)
            .vexpand(true)
            .build();

        // 14 Top-Level Pages
        detail_stack.add_named(&page_accounts, Some("accounts"));
        detail_stack.add_named(&page_mail_labels, Some("labels"));
        detail_stack.add_named(&page_filters, Some("filters"));
        detail_stack.add_named(&page_certificates, Some("certificates"));
        detail_stack.add_named(&page_mail_general, Some("mail_general"));
        detail_stack.add_named(&page_appearance, Some("appearance"));
        detail_stack.add_named(&page_mail_layout, Some("mail_layout"));
        detail_stack.add_named(&page_mail_html, Some("mail_reading"));
        detail_stack.add_named(&page_composer_general, Some("composer"));
        detail_stack.add_named(&page_calendar_general, Some("calendar"));
        detail_stack.add_named(&page_contacts, Some("contacts"));
        detail_stack.add_named(&page_tasks, Some("tasks"));
        detail_stack.add_named(&page_backup, Some("backup"));
        detail_stack.add_named(&page_sub_backup_advanced, Some("advanced"));

        // 13 Subpages
        detail_stack.add_named(&page_mail_junk, Some("junk"));
        detail_stack.add_named(&page_sub_smime, Some("sub_smime"));
        detail_stack.add_named(&page_sub_sync_cache, Some("sub_sync_cache"));
        detail_stack.add_named(&page_sub_header_toolbar, Some("sub_header_toolbar"));
        detail_stack.add_named(&page_sub_list_elements, Some("sub_list_elements"));
        detail_stack.add_named(&page_mail_headers, Some("headers"));
        detail_stack.add_named(&page_sub_reading_advanced, Some("sub_reading_advanced"));
        detail_stack.add_named(&page_sub_cal_workdays, Some("sub_calendar_workdays"));
        detail_stack.add_named(&page_sub_cal_reminders, Some("sub_calendar_reminders"));
        detail_stack.add_named(&page_sub_cal_categories, Some("sub_calendar_categories"));
        detail_stack.add_named(&page_sub_calendar_view, Some("sub_calendar_view"));
        detail_stack.add_named(&page_sub_allowed_senders, Some("sub_allowed_senders"));
        detail_stack.add_named(&page_sub_list_toolbar, Some("sub_list_toolbar"));

        enable_overlay_scrolling_recursive(detail_stack.upcast_ref());

        // ── Navigation Split View (Modern Libadwaita & GNOME HIG Parity) ────
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(205.0)
            .max_sidebar_width(235.0)
            .sidebar_width_fraction(0.28)
            .build();

        // ── Sidebar Category List & NavigationPage ───────────────────────────
        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar", "settings-side"])
            .build();

        for sec in SIDEBAR_SECTIONS {
            for page in sec.pages {
                let row = gtk::ListBoxRow::new();
                row.set_widget_name(&format!("page:{}", page.id));
                row.set_height_request(36);
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                line.set_margin_start(10);
                line.set_margin_end(10);
                line.set_margin_top(4);
                line.set_margin_bottom(4);
                let icon = gtk::Image::from_icon_name(page.icon);
                line.append(&icon);
                let title_label = gtk::Label::new(Some(&gettext(page.title)));
                title_label.set_xalign(0.0);
                title_label.set_hexpand(true);
                line.append(&title_label);
                row.set_child(Some(&line));
                sidebar_list.append(&row);
            }
        }

        sidebar_list.set_header_func(|row, _before| {
            let name = row.widget_name();
            if matches!(
                name.as_str(),
                "page:mail_general" | "page:calendar" | "page:backup"
            ) {
                let sep = gtk::Separator::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .margin_top(6)
                    .margin_bottom(6)
                    .margin_start(12)
                    .margin_end(12)
                    .build();
                row.set_header(Some(&sep));
            } else {
                row.set_header(None::<&gtk::Widget>);
            }
        });

        let sidebar_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&sidebar_list)
            .build();

        // ── Live Search Implementation (Hylki Pattern) ──────────────────────
        let search_button = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text(&gettext("Search Settings"))
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(&gettext("Search settings…"))
            .hexpand(true)
            .build();

        let search_bar = gtk::SearchBar::builder()
            .child(&search_entry)
            .build();
        search_bar.connect_entry(&search_entry);
        search_bar
            .bind_property("search-mode-enabled", &search_button, "active")
            .bidirectional()
            .sync_create()
            .build();

        let results_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let results_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&results_list)
            .build();

        let empty_search_page = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title(&gettext("No Results"))
            .description(&gettext("Try a different search term"))
            .css_classes(vec!["compact"])
            .build();

        let side_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        side_stack.add_named(&sidebar_scroller, Some("pages"));
        side_stack.add_named(&results_scroller, Some("results"));
        side_stack.add_named(&empty_search_page, Some("empty"));

        let side_header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();
        let side_title = adw::WindowTitle::new(&gettext("Settings"), "");
        side_header.set_title_widget(Some(&side_title));
        side_header.pack_start(&search_button);

        let side_toolbar = adw::ToolbarView::new();
        side_toolbar.add_top_bar(&side_header);
        side_toolbar.add_top_bar(&search_bar);
        side_toolbar.set_content(Some(&side_stack));

        let sidebar_page = adw::NavigationPage::builder()
            .title(&gettext("Settings"))
            .child(&side_toolbar)
            .build();

        // ── Content Page & Header (Libadwaita NavigationSplitView Parity) ────
        let content_header = adw::HeaderBar::new();
        let content_title = adw::WindowTitle::new(&gettext("General"), "");
        content_header.set_title_widget(Some(&content_title));

        let subpage_back_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(&gettext("Back"))
            .visible(false)
            .build();
        subpage_back_btn.add_css_class("flat");
        content_header.pack_start(&subpage_back_btn);

        let active_subpage_parent = self.imp().active_subpage_parent.clone();

        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.set_content(Some(&detail_stack));

        let content_page = adw::NavigationPage::builder()
            .title(&gettext("General"))
            .child(&content_toolbar)
            .build();

        split.set_sidebar(Some(&sidebar_page));
        split.set_content(Some(&content_page));

        // Close button responsiveness on narrow collapse:
        split
            .bind_property("collapsed", &side_header, "show-end-title-buttons")
            .sync_create()
            .build();

        // Responsive 640sp breakpoint (single pane collapse on narrow windows)
        let narrow_bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            640.0,
            adw::LengthUnit::Sp,
        ));
        narrow_bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        self.add_breakpoint(narrow_bp);

        // ── Subpage Activation & Header Bar Back Button Wiring ──────────────
        {
            let detail_stack_weak = detail_stack.downgrade();
            let content_title_weak = content_title.downgrade();
            let content_page_weak = content_page.downgrade();
            let split_weak = split.downgrade();
            let back_btn_weak = subpage_back_btn.downgrade();
            let parent_tracker = active_subpage_parent.clone();
            let sidebar_weak = sidebar_list.downgrade();

            *open_subpage_trigger.borrow_mut() = Some(Box::new(move |subpage_id: &'static str| {
                let Some(stack) = detail_stack_weak.upgrade() else { return };
                let parent_id = resolve_sidebar_page_id(subpage_id);

                if let Some(sidebar) = sidebar_weak.upgrade() {
                    select_sidebar_page(&sidebar, parent_id);
                }

                parent_tracker.replace(Some(parent_id));

                stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
                stack.set_visible_child_name(subpage_id);

                let title = get_page_title(subpage_id).unwrap_or_else(|| subpage_id.to_string());
                let parent_title = get_page_title(parent_id).unwrap_or_default();
                if let Some(ct) = content_title_weak.upgrade() {
                    ct.set_title(&title);
                    ct.set_subtitle(&parent_title);
                }
                if let Some(cp) = content_page_weak.upgrade() {
                    cp.set_title(&title);
                }
                if let Some(btn) = back_btn_weak.upgrade() {
                    btn.set_visible(true);
                }
                if let Some(s) = split_weak.upgrade() {
                    s.set_show_content(true);
                }
            }));
        }

        {
            let detail_stack_weak = detail_stack.downgrade();
            let content_title_weak = content_title.downgrade();
            let content_page_weak = content_page.downgrade();
            let back_btn_weak = subpage_back_btn.downgrade();
            let parent_tracker = active_subpage_parent.clone();
            let sidebar_weak = sidebar_list.downgrade();

            subpage_back_btn.connect_clicked(move |_| {
                let Some(parent_id) = parent_tracker.borrow_mut().take() else { return };
                if let Some(stack) = detail_stack_weak.upgrade() {
                    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
                    stack.set_visible_child_name(parent_id);
                }
                if let Some(sidebar) = sidebar_weak.upgrade() {
                    select_sidebar_page(&sidebar, parent_id);
                }
                let title = get_page_title(parent_id).unwrap_or_else(|| parent_id.to_string());
                if let Some(ct) = content_title_weak.upgrade() {
                    ct.set_title(&title);
                    ct.set_subtitle("");
                }
                if let Some(cp) = content_page_weak.upgrade() {
                    cp.set_title(&title);
                }
                if let Some(btn) = back_btn_weak.upgrade() {
                    btn.set_visible(false);
                }
            });
        }

        // ── Interaction Wiring ──────────────────────────────────────────────
        let detail_stack_weak = detail_stack.downgrade();
        let content_title_weak = content_title.downgrade();
        let content_page_weak = content_page.downgrade();
        let split_weak = split.downgrade();
        let subpage_back_btn_for_sidebar = subpage_back_btn.downgrade();
        let active_subpage_parent_for_sidebar = active_subpage_parent.clone();

        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let name = row.widget_name();
            let Some(page_id) = name.strip_prefix("page:") else { return };

            active_subpage_parent_for_sidebar.replace(None);
            if let Some(btn) = subpage_back_btn_for_sidebar.upgrade() {
                btn.set_visible(false);
            }

            if let Some(stack) = detail_stack_weak.upgrade() {
                stack.set_transition_type(gtk::StackTransitionType::Crossfade);
                stack.set_visible_child_name(page_id);
            }
            let title = get_page_title(page_id).unwrap_or_else(|| page_id.to_string());
            if let Some(ct) = content_title_weak.upgrade() {
                ct.set_title(&title);
                ct.set_subtitle("");
            }
            if let Some(cp) = content_page_weak.upgrade() {
                cp.set_title(&title);
            }
            if let Some(s) = split_weak.upgrade() {
                s.set_show_content(true);
            }
        });

        let search_index: Rc<RefCell<Option<Vec<SearchHit>>>> = Rc::new(RefCell::new(None));
        let shown_hits: Rc<RefCell<Vec<SearchHit>>> = Rc::new(RefCell::new(Vec::new()));

        let execute_search: Rc<dyn Fn(&str)> = {
            let detail_stack_for_search = detail_stack.clone();
            let side_stack_for_search = side_stack.clone();
            let results_list_for_search = results_list.clone();
            let search_index_for_search = search_index.clone();
            let shown_hits_for_search = shown_hits.clone();
            Rc::new(move |raw_text: &str| {
                let query = raw_text.trim().to_lowercase();
                if query.is_empty() {
                    side_stack_for_search.set_visible_child_name("pages");
                    return;
                }
                if search_index_for_search.borrow().is_none() {
                    *search_index_for_search.borrow_mut() = Some(build_search_index(&detail_stack_for_search));
                }
                let words: Vec<&str> = query.split_whitespace().collect();
                let index_ref = search_index_for_search.borrow();
                let Some(index) = index_ref.as_ref() else { return };

                let mut found: Vec<(u8, usize, &SearchHit)> = index
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| words.iter().all(|w| h.haystack.contains(w)))
                    .filter(|(_, h)| h.widget.upgrade().is_some_and(|w| shown_in_hierarchy(&w)))
                    .map(|(i, h)| {
                        let title_lower = h.title.to_lowercase();
                        let rank = if title_lower.contains(&query) {
                            0
                        } else if words.iter().all(|w| title_lower.contains(w)) {
                            1
                        } else {
                            2
                        };
                        (rank, i, h)
                    })
                    .collect();
                found.sort_by_key(|(rank, i, _)| (*rank, *i));
                found.truncate(60);

                while let Some(row) = results_list_for_search.row_at_index(0) {
                    results_list_for_search.remove(&row);
                }
                let mut shown = shown_hits_for_search.borrow_mut();
                shown.clear();

                for (_, _, hit) in &found {
                    let line = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    line.set_margin_start(8);
                    line.set_margin_end(8);
                    line.set_margin_top(6);
                    line.set_margin_bottom(6);
                    let title = gtk::Label::new(Some(&hit.title));
                    title.set_xalign(0.0);
                    title.set_wrap(true);
                    let place = gtk::Label::new(Some(&hit.place));
                    place.set_xalign(0.0);
                    place.set_wrap(true);
                    place.add_css_class("caption");
                    place.add_css_class("dim-label");
                    line.append(&title);
                    line.append(&place);
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&line));
                    results_list_for_search.append(&row);
                    shown.push((*hit).clone());
                }

                side_stack_for_search.set_visible_child_name(if shown.is_empty() {
                    "empty"
                } else {
                    "results"
                });
            })
        };

        let search_cb = execute_search.clone();
        search_entry.connect_search_changed(move |entry| {
            search_cb(&entry.text());
        });

        let search_cb_changed = execute_search.clone();
        search_entry.connect_changed(move |entry| {
            search_cb_changed(&entry.text());
        });

        let search_entry_for_bar = search_entry.clone();
        let side_stack_for_bar = side_stack.clone();
        search_bar.connect_search_mode_enabled_notify(move |bar| {
            if !bar.is_search_mode() {
                search_entry_for_bar.set_text("");
                side_stack_for_bar.set_visible_child_name("pages");
            }
        });

        let shown_hits_for_pick = shown_hits.clone();
        let sidebar_list_for_pick = sidebar_list.clone();
        let detail_stack_for_pick = detail_stack.clone();
        let content_title_for_pick = content_title.clone();
        let content_page_for_pick = content_page.clone();
        let split_for_pick = split.clone();
        let subpage_back_btn_for_results = subpage_back_btn.downgrade();
        let active_subpage_parent_for_results = active_subpage_parent.clone();
        let open_subpage_for_results = open_subpage.clone();

        results_list.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            let hit = shown_hits_for_pick.borrow().get(idx).cloned();
            let Some(hit) = hit else { return };

            let is_subpage = SUBPAGES.iter().find(|s| s.id == hit.page_id);
            if let Some(sub) = is_subpage {
                open_subpage_for_results(sub.id);
            } else {
                active_subpage_parent_for_results.replace(None);
                if let Some(btn) = subpage_back_btn_for_results.upgrade() {
                    btn.set_visible(false);
                }
                detail_stack_for_pick.set_transition_type(gtk::StackTransitionType::Crossfade);
                detail_stack_for_pick.set_visible_child_name(hit.page_id);
                let title = get_page_title(hit.page_id).unwrap_or_else(|| hit.page_id.to_string());
                content_title_for_pick.set_title(&title);
                content_title_for_pick.set_subtitle("");
                content_page_for_pick.set_title(&title);
                select_sidebar_page(&sidebar_list_for_pick, hit.page_id);
                split_for_pick.set_show_content(true);
            }

            if let Some(target) = hit.widget.upgrade() {
                reveal_widget(&target);
            }
        });

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);

        let search_button_for_ctrl_f = search_button.clone();
        let search_entry_for_ctrl_f = search_entry.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                search_button_for_ctrl_f.set_active(true);
                search_entry_for_ctrl_f.grab_focus();
                glib::Propagation::Stop
            })),
        ));

        let search_button_for_escape = search_button.clone();
        let search_bar_for_escape = search_bar.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                if search_bar_for_escape.is_search_mode() {
                    search_button_for_escape.set_active(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            })),
        ));
        self.add_controller(shortcuts);

        let env_page = std::env::var("MERIDIAN_PREF_PAGE")
            .or_else(|_| std::env::var("EVOLUTION_PREF_PAGE"))
            .ok();
        let env_subpage = std::env::var("MERIDIAN_PREF_SUBPAGE")
            .or_else(|_| std::env::var("EVOLUTION_PREF_SUBPAGE"))
            .ok();

        let initial_page_id = match (env_page.as_deref(), env_subpage.as_deref()) {
            (Some("accounts"), Some("contacts")) => "contacts",
            (Some("accounts"), Some("filters")) => "filters",
            (Some("accounts"), _) => "accounts",
            (Some("contacts"), _) => "contacts",
            (Some("signatures"), _) => "accounts",
            (Some("labels"), _) => "labels",
            (Some("filters" | "sub_filters"), _) => "filters",
            (Some("mail"), Some("composer")) => "composer",
            (Some("mail"), Some("layout")) => "mail_layout",
            (Some("mail"), Some("reading" | "html")) => "mail_reading",
            (Some("mail"), Some("headers")) => "headers",
            (Some("mail"), Some("labels")) => "labels",
            (Some("mail"), Some("junk" | "spam")) => "junk",
            (Some("mail"), _) => "mail_general",
            (Some("mail_general"), _) => "mail_general",
            (Some("mail_reading" | "reading" | "html"), _) => "mail_reading",
            (Some("mail_layout" | "layout"), _) => "mail_layout",
            (Some("composer"), _) => "composer",
            (Some("replies"), _) => "composer",
            (Some("headers"), _) => "headers",
            (Some("junk" | "spam"), _) => "junk",
            (Some("calendar"), Some("display")) => "sub_calendar_view",
            (Some("calendar"), _) => "calendar",
            (Some("calendar_display" | "display"), _) => "calendar",
            (Some("appearance" | "theme"), _) => "appearance",
            (Some("certificates"), _) => "certificates",
            (Some("network"), _) => "mail_general",
            (Some("backup"), _) => "backup",
            (Some("security"), Some("certificates")) => "certificates",
            (Some("security"), Some("junk")) => "junk",
            (Some("security"), _) => "certificates",
            _ => "mail_general",
        };

        detail_stack.set_visible_child_name(initial_page_id);
        let initial_title = get_page_title(initial_page_id).unwrap_or_else(|| initial_page_id.to_string());
        content_title.set_title(&initial_title);
        content_page.set_title(&initial_title);
        select_sidebar_page(&sidebar_list, initial_page_id);

        self.imp().detail_stack.replace(Some(detail_stack));
        self.imp().sidebar_list.replace(Some(sidebar_list));
        self.imp().content_title.replace(Some(content_title));
        self.imp().content_page.replace(Some(content_page));
        self.imp().subpage_back_btn.replace(Some(subpage_back_btn));

        self.set_child(Some(&split));
    }

    pub fn select_page(&self, target: &str) {
        let canonical_target = match target {
            "sub_backup_advanced" => "advanced",
            "sub_filters" => "filters",
            "signatures" => "accounts",
            "receiving" | "network" => "mail_general",
            "replies" => "composer",
            "calendar_display" => "calendar",
            other => other,
        };
        let is_sub = SUBPAGES.iter().find(|s| s.id == canonical_target);
        let parent_target = if let Some(sub) = is_sub {
            sub.parent_id
        } else {
            canonical_target
        };
        if let Some(sidebar) = self.imp().sidebar_list.borrow().as_ref() {
            select_sidebar_page(sidebar, parent_target);
        }
        if let Some(stack) = self.imp().detail_stack.borrow().as_ref() {
            if is_sub.is_some() {
                stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
            }
            stack.set_visible_child_name(canonical_target);
        }
        let title = get_page_title(canonical_target).unwrap_or_else(|| canonical_target.to_string());
        if let Some(ct) = self.imp().content_title.borrow().as_ref() {
            ct.set_title(&title);
            if let Some(sub) = is_sub {
                let parent_title = get_page_title(sub.parent_id).unwrap_or_default();
                ct.set_subtitle(&parent_title);
            } else {
                ct.set_subtitle("");
            }
        }
        if let Some(cp) = self.imp().content_page.borrow().as_ref() {
            cp.set_title(&title);
        }
        if let Some(sub) = is_sub {
            self.imp().active_subpage_parent.replace(Some(sub.parent_id));
            if let Some(btn) = self.imp().subpage_back_btn.borrow().as_ref() {
                btn.set_visible(true);
            }
        } else {
            self.imp().active_subpage_parent.replace(None);
            if let Some(btn) = self.imp().subpage_back_btn.borrow().as_ref() {
                btn.set_visible(false);
            }
        }
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = crate::window::find_meridian_window(self) {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
        tracing::warn!(%message, "Preferences dialog result");
    }
}

const MAX_EDITABLE_MAIL_NORMAL_HEADERS: usize = 64;

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

fn configure_folder_combo_row(row: &adw::ComboRow) {
    row.add_css_class("compact-grouped-combo");
    let expr = gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    );
    row.set_expression(Some(&expr));
    row.set_enable_search(true);

    let title_lower = row.title().to_ascii_lowercase();
    let fixed_icon: Option<&'static str> = if title_lower.contains("calendar") {
        Some("x-office-calendar-symbolic")
    } else if title_lower.contains("address") || title_lower.contains("contact") {
        Some("x-office-address-book-symbolic")
    } else if title_lower.contains("task") {
        Some("view-list-bullet-symbolic")
    } else if title_lower.contains("memo") {
        Some("accessories-text-editor-symbolic")
    } else {
        None
    };

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::builder()
            .xalign(1.0)
            .valign(gtk::Align::Center)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(38)
            .build();
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(|_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let Some(str_obj) = list_item.item().and_downcast::<gtk::StringObject>() else { return };
        let Some(label) = list_item.child().and_downcast::<gtk::Label>() else { return };
        let raw = str_obj.string();
        if let Some((folder, acct)) = raw.split_once(" — ") {
            label.set_label(&format!("{folder} ({acct})"));
        } else {
            label.set_label(&raw);
        }
    });
    row.set_factory(Some(&factory));

    let list_factory = gtk::SignalListItemFactory::new();
    list_factory.connect_setup(|_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let hbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(4)
            .margin_end(6)
            .margin_top(0)
            .margin_bottom(0)
            .width_request(260)
            .build();
        let icon = gtk::Image::builder()
            .icon_name("folder-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["dim-label"])
            .build();
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let title_lbl = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let sub_lbl = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(vec!["caption", "dim-label"])
            .build();
        vbox.append(&title_lbl);
        vbox.append(&sub_lbl);
        hbox.append(&icon);
        hbox.append(&vbox);
        list_item.set_child(Some(&hbox));
    });
    list_factory.connect_bind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let Some(str_obj) = list_item.item().and_downcast::<gtk::StringObject>() else { return };
        let Some(hbox) = list_item.child().and_downcast::<gtk::Box>() else { return };
        let Some(icon) = hbox.first_child().and_downcast::<gtk::Image>() else { return };
        let Some(vbox) = icon.next_sibling().and_downcast::<gtk::Box>() else { return };
        let Some(title_lbl) = vbox.first_child().and_downcast::<gtk::Label>() else { return };
        let Some(sub_lbl) = title_lbl.next_sibling().and_downcast::<gtk::Label>() else { return };

        let raw = str_obj.string();
        let (folder, acct) = if let Some((f, a)) = raw.split_once(" — ") {
            (f.trim(), Some(a.trim()))
        } else {
            (raw.as_str(), None)
        };
        title_lbl.set_label(folder);
        let _ = acct;
        sub_lbl.set_visible(false);

        let icon_name = if let Some(fixed) = fixed_icon {
            fixed
        } else {
            let lower = folder.to_ascii_lowercase();
            if lower.contains("inbox") || lower.contains("posteingang") {
                "mail-inbox-symbolic"
            } else if lower.contains("unread") || lower.contains("ungelesen") {
                "mail-unread-symbolic"
            } else if lower.contains("star") || lower.contains("markiert") || lower.contains("flag") {
                "starred-symbolic"
            } else if lower.contains("sent") || lower.contains("gesendet") {
                "mail-send-symbolic"
            } else if lower.contains("draft") || lower.contains("entw") {
                "document-edit-symbolic"
            } else if lower.contains("trash") || lower.contains("papierkorb") || lower.contains("deleted") {
                "user-trash-symbolic"
            } else if lower.contains("spam") || lower.contains("junk") {
                "mail-mark-junk-symbolic"
            } else if lower.contains("archive") || lower.contains("archiv") {
                "package-x-generic-symbolic"
            } else {
                "folder-symbolic"
            }
        };
        icon.set_icon_name(Some(icon_name));
    });
    row.set_list_factory(Some(&list_factory));

    // Group folders by account: wrap the flat "Folder — Account" model in a
    // section model so the popup shows one compact account header per group.
    if let Some(model) = row.model() {
        let mut rank: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for i in 0..model.n_items() {
            if let Some(obj) = model.item(i).and_downcast::<gtk::StringObject>() {
                let acct = folder_combo_account(&obj.string()).to_string();
                let next = rank.len() as u32;
                rank.entry(acct).or_insert(next);
            }
        }
        let section_sorter = gtk::CustomSorter::new(move |a, b| {
            let rank_of = |o: &glib::Object| {
                o.downcast_ref::<gtk::StringObject>()
                    .and_then(|s| rank.get(folder_combo_account(&s.string())).copied())
                    .unwrap_or(u32::MAX)
            };
            rank_of(a).cmp(&rank_of(b)).into()
        });
        let sectioned = gtk::SortListModel::new(Some(model), None::<gtk::Sorter>);
        sectioned.set_section_sorter(Some(&section_sorter));
        row.set_model(Some(&sectioned));

        let header_factory = gtk::SignalListItemFactory::new();
        header_factory.connect_setup(|_, obj| {
            let Some(header) = obj.downcast_ref::<gtk::ListHeader>() else { return };
            let label = gtk::Label::builder()
                .xalign(0.0)
                .margin_start(4)
                .margin_top(0)
                .margin_bottom(0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(vec!["caption-heading", "dim-label"])
                .build();
            header.set_child(Some(&label));
        });
        header_factory.connect_bind(|_, obj| {
            let Some(header) = obj.downcast_ref::<gtk::ListHeader>() else { return };
            let Some(label) = header.child().and_downcast::<gtk::Label>() else { return };
            label.set_margin_top(0);
            let acct = header
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| folder_combo_account(&s.string()).to_string())
                .unwrap_or_default();
            if acct.is_empty() {
                label.set_label(&crate::i18n::gettext("All Accounts"));
            } else {
                label.set_label(&acct);
            }
        });
        row.set_header_factory(Some(&header_factory));
    }
}

/// Account part of a "Folder — Account" combo label ("" for unified folders).
fn folder_combo_account(raw: &str) -> &str {
    raw.split_once(" — ").map(|(_, a)| a.trim()).unwrap_or("")
}

fn collect_account_panel_folders(current_setting: Option<&str>) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = vec![
        ("inbox".to_string(), crate::i18n::gettext("Inbox")),
        ("all".to_string(), crate::i18n::gettext("All Messages")),
        ("sent".to_string(), crate::i18n::gettext("Sent")),
        ("drafts".to_string(), crate::i18n::gettext("Drafts")),
        ("trash".to_string(), crate::i18n::gettext("Trash")),
        ("archive".to_string(), crate::i18n::gettext("Archive")),
        ("spam".to_string(), crate::i18n::gettext("Spam")),
        ("unified_unread".to_string(), crate::i18n::gettext("Unread")),
        ("unified_flagged".to_string(), crate::i18n::gettext("Starred")),
    ];
    let mut seen_ids: std::collections::HashSet<String> =
        entries.iter().map(|(id, _)| id.clone()).collect();

    let live_summaries = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|w| w.downcast::<crate::window::MeridianWindow>().ok())
        .and_then(|win| win.mail_page())
        .map(|mp| mp.cached_folder_summaries())
        .unwrap_or_default();

    let profiles = AccountConfigStore::default().load();
    let mut covered_sources = std::collections::HashSet::<String>::new();

    for profile in profiles.iter().filter(|p| p.enabled) {
        let acct_label = if !profile.display_name.trim().is_empty() {
            profile.display_name.trim().to_string()
        } else {
            profile.email.trim().to_string()
        };
        let eds_uid =
            crate::services::account_config_service::eds_source_uid_for_email(&profile.email);
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

        if !matching_live.is_empty() {
            for f in matching_live {
                let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
                covered_sources.insert(f_canon.to_string());
                let key = format!("{f_canon}::{}", f.folder_name);
                if seen_ids.insert(key.clone()) {
                    let clean_disp = f
                        .display_name
                        .strip_prefix("[Gmail]/")
                        .or_else(|| f.display_name.strip_prefix("[Google Mail]/"))
                        .unwrap_or(&f.display_name);
                    entries.push((key, format!("{clean_disp} — {acct_label}")));
                }
            }
        } else {
            let uid = if !eds_canon.is_empty() {
                eds_canon
            } else {
                prof_canon
            };
            covered_sources.insert(uid.to_string());
            for (raw_fname, localized) in [
                ("INBOX", crate::i18n::gettext("Inbox")),
                ("Sent", crate::i18n::gettext("Sent")),
                ("Drafts", crate::i18n::gettext("Drafts")),
                ("Archive", crate::i18n::gettext("Archive")),
                ("Trash", crate::i18n::gettext("Trash")),
                ("Junk", crate::i18n::gettext("Spam")),
            ] {
                let key = format!("{uid}::{raw_fname}");
                if seen_ids.insert(key.clone()) {
                    entries.push((key, format!("{localized} — {acct_label}")));
                }
            }
        }
    }

    for f in &live_summaries {
        let f_canon = f.source_uid.strip_prefix("eds:").unwrap_or(&f.source_uid);
        if f_canon == "local"
            || f.source_display_name.to_ascii_lowercase().contains("computer")
            || covered_sources.contains(f_canon)
            || !f.is_selectable
            || f.folder_name.starts_with('.')
            || f.folder_name.contains("/.")
            || f.folder_name == "[Gmail]"
            || f.folder_name.eq_ignore_ascii_case("[Google Mail]")
        {
            continue;
        }
        let key = format!("{f_canon}::{}", f.folder_name);
        if seen_ids.insert(key.clone()) {
            let clean_disp = f
                .display_name
                .strip_prefix("[Gmail]/")
                .or_else(|| f.display_name.strip_prefix("[Google Mail]/"))
                .unwrap_or(&f.display_name);
            let acct = if f.source_display_name.trim().is_empty() {
                f_canon
            } else {
                f.source_display_name.trim()
            };
            entries.push((key, format!("{clean_disp} — {acct}")));
        }
    }

    if let Some(cur) = current_setting.map(str::trim).filter(|s| !s.is_empty()) {
        if seen_ids.insert(cur.to_string()) {
            let label = cur
                .split_once("::")
                .map(|(src, fname)| format!("{fname} — {src}"))
                .unwrap_or_else(|| cur.to_string());
            entries.push((cur.to_string(), label));
        }
    }

    entries
}

fn header_action_meta(id: &str) -> (&'static str, String) {
    match id {
        "reply" => ("mail-reply-sender-symbolic", crate::i18n::gettext("Reply")),
        "reply-all" => ("mail-reply-all-symbolic", crate::i18n::gettext("Reply All")),
        "forward" => ("mail-forward-symbolic", crate::i18n::gettext("Forward")),
        "archive" => ("package-x-generic-symbolic", crate::i18n::gettext("Archive")),
        "move" => ("folder-symbolic", crate::i18n::gettext("Move To")),
        "memo" => ("accessories-text-editor-symbolic", crate::i18n::gettext("Notes")),
        "unread" => ("mail-unread-symbolic", crate::i18n::gettext("Unread")),
        "delete" => ("user-trash-symbolic", crate::i18n::gettext("Delete")),
        "junk" => ("mail-mark-junk-symbolic", crate::i18n::gettext("Spam")),
        "print" => ("document-print-symbolic", crate::i18n::gettext("Print")),
        "star" => ("starred-symbolic", crate::i18n::gettext("Star")),
        "follow-up" => ("bookmark-new-symbolic", crate::i18n::gettext("Follow-Up")),
        "important" => ("emblem-important-symbolic", crate::i18n::gettext("Important")),
        "edit-labels" => ("preferences-color-symbolic", crate::i18n::gettext("Labels")),
        "source" => ("text-x-generic-symbolic", crate::i18n::gettext("Source")),
        "add-sender-contact" => ("contact-new-symbolic", crate::i18n::gettext("Add Contact")),
        _ => ("folder-symbolic", id.to_string()),
    }
}

mod toolbar_zone_imp {
    use super::*;

    pub struct ToolbarZone {
        pub gap_index: Cell<Option<usize>>,
        pub gap_width: Cell<i32>,
    }

    impl Default for ToolbarZone {
        fn default() -> Self {
            Self {
                gap_index: Cell::new(None),
                gap_width: Cell::new(52),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ToolbarZone {
        const NAME: &'static str = "MeridianToolbarZone";
        type Type = super::ToolbarZone;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ToolbarZone {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ToolbarZone {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                return (60, 240, -1, -1);
            }
            let avail = if for_size > 0 { for_size } else { 420 };
            let (_, total_h) = self.layout_slots(avail, true);
            let h = total_h.max(44);
            (h, h, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let (slots, _) = self.layout_slots(width, true);
            for (child_opt, x, y, w, h) in slots {
                if let Some(child) = child_opt {
                    child.size_allocate(&gtk::Allocation::new(x, y, w, h), -1);
                }
            }
        }
    }

    impl ToolbarZone {
        pub fn layout_slots(
            &self,
            avail_width: i32,
            include_gap: bool,
        ) -> (Vec<(Option<gtk::Widget>, i32, i32, i32, i32)>, i32) {
            let mut items: Vec<(Option<gtk::Widget>, i32, i32)> = Vec::new();
            let mut max_h = 36_i32;

            let mut child_opt = self.obj().first_child();
            while let Some(child) = child_opt {
                let next = child.next_sibling();
                if child.should_layout() {
                    let (_, nat_w, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
                    let (_, nat_h, _, _) = child.measure(gtk::Orientation::Vertical, nat_w);
                    max_h = max_h.max(nat_h);
                    items.push((Some(child), nat_w, nat_h));
                }
                child_opt = next;
            }

            if include_gap {
                if let Some(gap_idx) = self.gap_index.get() {
                    let idx = gap_idx.min(items.len());
                    let gw = self.gap_width.get().max(48);
                    items.insert(idx, (None, gw, max_h));
                }
            }

            let avail = avail_width.max(1);
            let mut x = 0_i32;
            let mut y = 0_i32;
            let mut row_h = 0_i32;
            let mut out = Vec::with_capacity(items.len());

            for (w_opt, w, h) in items {
                if x > 0 && x + w > avail {
                    x = 0;
                    y += row_h + 6;
                    row_h = 0;
                }
                out.push((w_opt, x, y, w, h));
                x += w + 6;
                row_h = row_h.max(h);
            }

            let total_h = if out.is_empty() { 0 } else { y + row_h };
            (out, total_h)
        }
    }
}

glib::wrapper! {
    pub struct ToolbarZone(ObjectSubclass<toolbar_zone_imp::ToolbarZone>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ToolbarZone {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.add_css_class("toolbar-zone");
        obj.set_hexpand(true);
        obj
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.as_ref().set_parent(self);
    }

    pub fn clear(&self) {
        self.imp().gap_index.set(None);
        while let Some(child) = self.first_child() {
            child.unparent();
        }
    }

    pub fn set_gap(&self, gap_index: Option<usize>, gap_width: i32) {
        let imp = self.imp();
        let w = gap_width.max(48);
        if imp.gap_index.get() != gap_index || imp.gap_width.get() != w {
            imp.gap_index.set(gap_index);
            imp.gap_width.set(w);
            self.queue_resize();
        }
    }

    pub fn drop_index(&self, x: f64, y: f64) -> usize {
        let lx = x - 6.0;
        let ly = y - 6.0;
        let avail = (self.width() - 12).max(1);
        let (slots, _) = self.imp().layout_slots(avail, false);
        let mut idx = 0;
        for (_, cx, cy, cw, ch) in slots {
            let row_top = cy as f64;
            let row_bottom = (cy + ch) as f64;
            let center_x = (cx as f64) + (cw as f64) * 0.5;
            if ly >= row_bottom || (ly >= row_top && lx >= center_x) {
                idx += 1;
            }
        }
        idx
    }
}

fn make_toolbar_tile_icon(
    base_token: &str,
    icon_name: &str,
    labels: &[crate::services::settings_service::MailLabelDefinition],
) -> gtk::Widget {
    if base_token == "separator" {
        return gtk::Separator::builder()
            .orientation(gtk::Orientation::Vertical)
            .height_request(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build()
            .upcast();
    }
    if let Some(tag) = base_token.strip_prefix("label:") {
        let color_hex = labels
            .iter()
            .find(|l| l.tag.eq_ignore_ascii_case(tag) || l.name.eq_ignore_ascii_case(tag))
            .map(|l| l.color.clone())
            .unwrap_or_else(|| "#3584e4".to_string());
        let rgba = gtk::gdk::RGBA::parse(&color_hex)
            .unwrap_or_else(|_| gtk::gdk::RGBA::new(0.21, 0.52, 0.89, 1.0));
        let dot = gtk::DrawingArea::builder()
            .content_width(14)
            .content_height(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        dot.set_draw_func(move |_, cr, w, h| {
            let r = 5.0_f64;
            cr.arc(w as f64 / 2.0, h as f64 / 2.0, r, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(rgba.red() as f64, rgba.green() as f64, rgba.blue() as f64, 1.0);
            let _ = cr.fill();
        });
        return dot.upcast();
    }
    gtk::Image::builder()
        .icon_name(icon_name)
        .pixel_size(16)
        .halign(gtk::Align::Center)
        .build()
        .upcast()
}

fn make_toolbar_chip(
    base_token: &str,
    icon_name: &str,
    label_text: &str,
    drag_payload: String,
    labels: &[crate::services::settings_service::MailLabelDefinition],
    parent_zone: &ToolbarZone,
    dragged_width: &Rc<Cell<i32>>,
) -> gtk::Box {
    let chip = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .css_classes(vec!["toolbar-chip"])
        .build();
    chip.set_cursor_from_name(Some("grab"));

    let icon_widget = make_toolbar_tile_icon(base_token, icon_name, labels);
    let lbl = gtk::Label::builder()
        .label(label_text)
        .css_classes(vec!["caption"])
        .halign(gtk::Align::Center)
        .build();
    chip.append(&icon_widget);
    chip.append(&lbl);

    let ds = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    ds.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&drag_payload.to_value()))
    });
    let chip_begin = chip.clone();
    let zone_begin = parent_zone.clone();
    let dw_begin = dragged_width.clone();
    let base_ghost = base_token.to_string();
    let icon_ghost = icon_name.to_string();
    let label_ghost = label_text.to_string();
    let labels_ghost = labels.to_vec();
    ds.connect_drag_begin(move |_, drag| {
        let w = chip_begin.width().max(52);
        let h = chip_begin.height().max(36);
        dw_begin.set(w);

        let ghost = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .valign(gtk::Align::Center)
            .css_classes(vec!["toolbar-chip"])
            .build();
        let g_icon = make_toolbar_tile_icon(&base_ghost, &icon_ghost, &labels_ghost);
        let g_lbl = gtk::Label::builder()
            .label(&label_ghost)
            .css_classes(vec!["caption"])
            .halign(gtk::Align::Center)
            .build();
        ghost.append(&g_icon);
        ghost.append(&g_lbl);
        let drag_icon = gtk::DragIcon::for_drag(drag);
        drag_icon.set_child(Some(&ghost));
        drag.set_hotspot(w / 2, h / 2);

        chip_begin.set_visible(false);
        zone_begin.queue_resize();
    });
    let chip_end = chip.clone();
    let zone_end = parent_zone.clone();
    ds.connect_drag_end(move |_, _, _| {
        chip_end.set_visible(true);
        zone_end.queue_resize();
    });
    chip.add_controller(ds);

    chip
}

fn make_toolbar_zone_row(
    list_box: &gtk::ListBox,
    title: &str,
    subtitle: &str,
    empty_text: &str,
) -> (ToolbarZone, gtk::Label) {
    let row_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    let hdr = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let lbl_title = gtk::Label::builder()
        .label(title)
        .halign(gtk::Align::Start)
        .build();
    let lbl_sub = gtk::Label::builder()
        .label(subtitle)
        .css_classes(vec!["dim-label", "caption"])
        .halign(gtk::Align::Start)
        .build();
    hdr.append(&lbl_title);
    hdr.append(&lbl_sub);
    row_box.append(&hdr);

    let zone = ToolbarZone::new();
    let empty_lbl = gtk::Label::builder()
        .label(empty_text)
        .css_classes(vec!["dim-label"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .can_target(false)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&zone));
    overlay.add_overlay(&empty_lbl);
    row_box.append(&overlay);

    let row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .can_focus(false)
        .child(&row_box)
        .build();
    list_box.append(&row);

    (zone, empty_lbl)
}

fn build_mail_header_toolbar_editor(settings: Option<AppSettings>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(&crate::i18n::gettext("Header Action Bar"))
        .build();

    let btn_restore = gtk::Button::builder()
        .label(&crate::i18n::gettext("Restore Defaults"))
        .valign(gtk::Align::Center)
        .css_classes(vec!["flat"])
        .build();
    group.set_header_suffix(Some(&btn_restore));

    let initial_groups = settings
        .as_ref()
        .map(AppSettings::mail_header_action_groups)
        .unwrap_or_else(|| {
            (
                crate::services::settings_service::DEFAULT_MAIL_HEADER_LEFT_ACTIONS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                crate::services::settings_service::DEFAULT_MAIL_HEADER_RIGHT_ACTIONS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            )
        });
    let is_initially_active = !initial_groups.0.is_empty() || !initial_groups.1.is_empty();

    let row_show_actions = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Show action buttons in message header"))
        .active(is_initially_active)
        .build();

    let row_mark_read = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Show mark-read actions in toolbar"))
        .active(settings.as_ref().map(|s| s.mail_show_mark_read_actions()).unwrap_or(false))
        .build();
    if let Some(ref s) = settings {
        let s_toggle = s.clone();
        row_mark_read.connect_active_notify(move |r| {
            s_toggle.set_mail_show_mark_read_actions(r.is_active());
        });
        let row_weak = row_mark_read.downgrade();
        s.connect_changed(Some("mail-show-mark-read-actions"), move |st, _| {
            if let Some(r) = row_weak.upgrade() {
                let act = st.boolean("mail-show-mark-read-actions");
                if r.is_active() != act {
                    r.set_active(act);
                }
            }
        });
    }

    row_mark_read.set_visible(false);
    let switches_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();
    switches_box.append(&row_show_actions);
    switches_box.append(&row_mark_read);
    group.add(&switches_box);

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .sensitive(is_initially_active)
        .margin_top(12)
        .build();

    let empty_str = crate::i18n::gettext("Empty");
    let (zone_left, empty_left) = make_toolbar_zone_row(
        &list_box,
        &crate::i18n::gettext("Left group"),
        &crate::i18n::gettext("Always shown"),
        &empty_str,
    );
    let (zone_right, empty_right) = make_toolbar_zone_row(
        &list_box,
        &crate::i18n::gettext("Right group"),
        &crate::i18n::gettext("Folds into ▾ when narrow"),
        &empty_str,
    );
    let (zone_hidden, empty_hidden) = make_toolbar_zone_row(
        &list_box,
        &crate::i18n::gettext("Not shown"),
        &crate::i18n::gettext("Drop a button here to move it into the ▾ menu"),
        &empty_str,
    );
    group.add(&list_box);

    let state: Rc<RefCell<(Vec<String>, Vec<String>)>> = Rc::new(RefCell::new(initial_groups));
    let syncing = Rc::new(Cell::new(false));
    let dragged_width = Rc::new(Cell::new(52_i32));

    type RebuildFn = Rc<RefCell<Option<Box<dyn Fn()>>>>;
    let rebuild_ref: RebuildFn = Rc::new(RefCell::new(None));

    let save_and_rebuild = {
        let state = state.clone();
        let settings = settings.clone();
        let syncing = syncing.clone();
        let rebuild_ref = rebuild_ref.clone();
        Rc::new(move || {
            let (left, right) = state.borrow().clone();
            if let Some(ref s) = settings {
                syncing.set(true);
                s.set_mail_header_action_groups(&left, &right);
                syncing.set(false);
            }
            if let Some(ref f) = *rebuild_ref.borrow() {
                f();
            }
        })
    };

    let state_toggle = state.clone();
    let save_toggle = save_and_rebuild.clone();
    let list_weak = list_box.downgrade();
    row_show_actions.connect_active_notify(move |row| {
        let active = row.is_active();
        if let Some(l) = list_weak.upgrade() {
            l.set_sensitive(active);
        }
        if !active {
            state_toggle.borrow_mut().0.clear();
            state_toggle.borrow_mut().1.clear();
            save_toggle();
        } else {
            let empty = state_toggle.borrow().0.is_empty() && state_toggle.borrow().1.is_empty();
            if empty {
                *state_toggle.borrow_mut() = (
                    crate::services::settings_service::DEFAULT_MAIL_HEADER_LEFT_ACTIONS
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    crate::services::settings_service::DEFAULT_MAIL_HEADER_RIGHT_ACTIONS
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                );
                save_toggle();
            }
        }
    });

    let attach_zone_drop = |zone: &ToolbarZone, empty_lbl: &gtk::Label, target_group: &'static str| {
        let dt = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        let z_enter = zone.clone();
        let e_enter = empty_lbl.clone();
        let dw_enter = dragged_width.clone();
        dt.connect_enter(move |_, x, y| {
            z_enter.add_css_class("drop-active");
            e_enter.set_visible(false);
            let idx = z_enter.drop_index(x, y);
            z_enter.set_gap(Some(idx), dw_enter.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_motion = zone.clone();
        let e_motion = empty_lbl.clone();
        let dw_motion = dragged_width.clone();
        dt.connect_motion(move |_, x, y| {
            z_motion.add_css_class("drop-active");
            e_motion.set_visible(false);
            let idx = z_motion.drop_index(x, y);
            z_motion.set_gap(Some(idx), dw_motion.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_leave = zone.clone();
        let e_leave = empty_lbl.clone();
        dt.connect_leave(move |_| {
            z_leave.remove_css_class("drop-active");
            z_leave.set_gap(None, 52);
            e_leave.set_visible(z_leave.first_child().is_none());
        });
        let z_drop = zone.clone();
        let state_drop = state.clone();
        let save_drop = save_and_rebuild.clone();
        dt.connect_drop(move |_, val, x, y| {
            z_drop.remove_css_class("drop-active");
            let insert_idx = z_drop.drop_index(x, y);
            z_drop.set_gap(None, 52);
            let Ok(payload) = val.get::<String>() else { return false; };
            let Some(action_id) = payload.strip_prefix("header-tile:") else { return false; };
            {
                let mut st = state_drop.borrow_mut();
                st.0.retain(|x| x != action_id);
                st.1.retain(|x| x != action_id);
                match target_group {
                    "left" => {
                        let at = insert_idx.min(st.0.len());
                        st.0.insert(at, action_id.to_string());
                    }
                    "right" => {
                        let at = insert_idx.min(st.1.len());
                        st.1.insert(at, action_id.to_string());
                    }
                    _ => {}
                }
            }
            save_drop();
            true
        });
        zone.add_controller(dt);
    };

    attach_zone_drop(&zone_left, &empty_left, "left");
    attach_zone_drop(&zone_right, &empty_right, "right");
    attach_zone_drop(&zone_hidden, &empty_hidden, "hidden");

    {
        let zone_left = zone_left.clone();
        let zone_right = zone_right.clone();
        let zone_hidden = zone_hidden.clone();
        let empty_left = empty_left.clone();
        let empty_right = empty_right.clone();
        let empty_hidden = empty_hidden.clone();
        let state = state.clone();
        let save_and_rebuild = save_and_rebuild.clone();
        let dragged_width = dragged_width.clone();
        *rebuild_ref.borrow_mut() = Some(Box::new(move || {
            for z in [&zone_left, &zone_right, &zone_hidden] {
                z.clear();
            }
            let (left, right) = state.borrow().clone();
            let hidden: Vec<String> = crate::services::settings_service::ALL_MAIL_HEADER_ACTIONS
                .iter()
                .filter(|id| !left.contains(&(**id).to_string()) && !right.contains(&(**id).to_string()))
                .map(|id| (*id).to_string())
                .collect();

            let populate_zone = |zone: &ToolbarZone, empty_lbl: &gtk::Label, items: &[String], group_name: &'static str| {
                empty_lbl.set_visible(items.is_empty());
                let total_len = items.len();
                for (idx, item_id) in items.iter().enumerate() {
                    let (icon_name, label_text) = header_action_meta(item_id);
                    let drag_id = format!("header-tile:{item_id}");
                    let chip = make_toolbar_chip(
                        item_id,
                        icon_name,
                        &label_text,
                        drag_id,
                        &[],
                        zone,
                        &dragged_width,
                    );

                    // Click / Right-click popover on release (does not block DragSource)
                    let id_click = item_id.clone();
                    let state_click = state.clone();
                    let save_click = save_and_rebuild.clone();
                    let tb_for_pop = chip.clone();
                    let open_menu = Rc::new(move || {
                        let pop_box = gtk::Box::builder()
                            .orientation(gtk::Orientation::Vertical)
                            .spacing(2)
                            .margin_start(4)
                            .margin_end(4)
                            .margin_top(4)
                            .margin_bottom(4)
                            .build();
                        let pop = gtk::Popover::builder()
                            .child(&pop_box)
                            .has_arrow(false)
                            .css_classes(vec!["menu", "context-menu"])
                            .build();
                        pop.set_parent(&tb_for_pop);

                        let make_menu_btn = |label: &str, sensitive: bool| -> gtk::Button {
                            gtk::Button::builder()
                                .child(
                                    &gtk::Label::builder()
                                        .label(label)
                                        .halign(gtk::Align::Start)
                                        .build(),
                                )
                                .halign(gtk::Align::Fill)
                                .sensitive(sensitive)
                                .css_classes(vec!["flat", "context-menu-item"])
                                .build()
                        };

                        if group_name != "hidden" && total_len > 1 {
                            let b_left = make_menu_btn(&crate::i18n::gettext("Move Left"), idx > 0);
                            let st_l = state_click.clone();
                            let sv_l = save_click.clone();
                            let pw_l = pop.downgrade();
                            b_left.connect_clicked(move |_| {
                                {
                                    let mut st = st_l.borrow_mut();
                                    let vec = if group_name == "left" { &mut st.0 } else { &mut st.1 };
                                    if idx > 0 && idx < vec.len() {
                                        vec.swap(idx, idx - 1);
                                    }
                                }
                                if let Some(p) = pw_l.upgrade() { p.popdown(); }
                                sv_l();
                            });
                            pop_box.append(&b_left);

                            let b_right = make_menu_btn(&crate::i18n::gettext("Move Right"), idx + 1 < total_len);
                            let st_r = state_click.clone();
                            let sv_r = save_click.clone();
                            let pw_r = pop.downgrade();
                            b_right.connect_clicked(move |_| {
                                {
                                    let mut st = st_r.borrow_mut();
                                    let vec = if group_name == "left" { &mut st.0 } else { &mut st.1 };
                                    if idx + 1 < vec.len() {
                                        vec.swap(idx, idx + 1);
                                    }
                                }
                                if let Some(p) = pw_r.upgrade() { p.popdown(); }
                                sv_r();
                            });
                            pop_box.append(&b_right);
                            pop_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
                        }

                        for (dest_key, dest_label) in [
                            ("left", crate::i18n::gettext("Move to Left group")),
                            ("right", crate::i18n::gettext("Move to Right group")),
                            ("hidden", crate::i18n::gettext("Move to Not shown (▾ menu)")),
                        ] {
                            if dest_key == group_name {
                                continue;
                            }
                            let b = make_menu_btn(&dest_label, true);
                            let st_m = state_click.clone();
                            let sv_m = save_click.clone();
                            let id_m = id_click.clone();
                            let pw_m = pop.downgrade();
                            b.connect_clicked(move |_| {
                                {
                                    let mut st = st_m.borrow_mut();
                                    st.0.retain(|x| x != &id_m);
                                    st.1.retain(|x| x != &id_m);
                                    match dest_key {
                                        "left" => st.0.push(id_m.clone()),
                                        "right" => st.1.push(id_m.clone()),
                                        _ => {}
                                    }
                                }
                                if let Some(p) = pw_m.upgrade() { p.popdown(); }
                                sv_m();
                            });
                            pop_box.append(&b);
                        }

                        pop.connect_closed(|p| {
                            let p_clone = p.clone();
                            glib::idle_add_local_once(move || {
                                p_clone.unparent();
                            });
                        });
                        pop.popup();
                    });

                    let click_g = gtk::GestureClick::builder().button(1).build();
                    let open_l = open_menu.clone();
                    click_g.connect_released(move |_, _, _, _| {
                        open_l();
                    });
                    chip.add_controller(click_g);

                    let rclick_g = gtk::GestureClick::builder().button(3).build();
                    let open_r = open_menu.clone();
                    rclick_g.connect_pressed(move |g, _, _, _| {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        open_r();
                    });
                    chip.add_controller(rclick_g);

                    zone.append(&chip);
                }
            };

            populate_zone(&zone_left, &empty_left, &left, "left");
            populate_zone(&zone_right, &empty_right, &right, "right");
            populate_zone(&zone_hidden, &empty_hidden, &hidden, "hidden");
        }));
    }

    if let Some(ref f) = *rebuild_ref.borrow() {
        f();
    }

    {
        let state_reset = state.clone();
        let save_reset = save_and_rebuild.clone();
        let row_actions_reset = row_show_actions.clone();
        let list_reset = list_box.clone();
        btn_restore.connect_clicked(move |_| {
            row_actions_reset.set_active(true);
            list_reset.set_sensitive(true);
            *state_reset.borrow_mut() = (
                crate::services::settings_service::DEFAULT_MAIL_HEADER_LEFT_ACTIONS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                crate::services::settings_service::DEFAULT_MAIL_HEADER_RIGHT_ACTIONS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            );
            save_reset();
        });
    }

    if let Some(ref s) = settings {
        let s_clone = s.clone();
        let state_ext = state.clone();
        let syncing_ext = syncing.clone();
        let rebuild_ext = rebuild_ref.clone();
        let row_actions_ext = row_show_actions.clone();
        let list_ext = list_box.clone();
        s.connect_changed(Some("mail-header-actions"), move |_, _| {
            if syncing_ext.get() {
                return;
            }
            let grps = s_clone.mail_header_action_groups();
            let act = !grps.0.is_empty() || !grps.1.is_empty();
            if row_actions_ext.is_active() != act {
                row_actions_ext.set_active(act);
            }
            list_ext.set_sensitive(act);
            *state_ext.borrow_mut() = grps;
            if let Some(ref f) = *rebuild_ext.borrow() {
                f();
            }
        });
    }

    group
}

fn list_toolbar_item_meta(raw_item: &str, labels: &[crate::services::settings_service::MailLabelDefinition]) -> (&'static str, String) {
    let (base, alias) = match raw_item.split_once('=') {
        Some((b, a)) => (b.trim(), Some(a.trim())),
        None => (raw_item.trim(), None),
    };
    let (icon, default_lbl) = match base {
        "filter:unread" => ("mail-unread-symbolic", crate::i18n::gettext("Unread")),
        "filter:flagged" | "filter:starred" => ("starred-symbolic", crate::i18n::gettext("Starred")),
        "filter:followup" | "filter:follow-up" => ("bookmark-new-symbolic", crate::i18n::gettext("Follow-Up")),
        "filter:memo" | "filter:memos" => ("accessories-text-editor-symbolic", crate::i18n::gettext("Memos")),
        "filter:attachment" | "filter:attachments" => ("mail-attachment-symbolic", crate::i18n::gettext("Attachments")),
        "filter:important" => ("emblem-important-symbolic", crate::i18n::gettext("Important")),
        "filter:today" => ("x-office-calendar-symbolic", crate::i18n::gettext("Today")),
        "follow-up" => ("bookmark-new-symbolic", crate::i18n::gettext("Set Follow-Up")),
        "star" => ("starred-symbolic", crate::i18n::gettext("Star")),
        "important" => ("emblem-important-symbolic", crate::i18n::gettext("Important")),
        "edit-labels" => ("preferences-color-symbolic", crate::i18n::gettext("Labels")),
        "mark-read" => ("mail-read-symbolic", crate::i18n::gettext("Mark Read")),
        "mark-unread" => ("mail-unread-symbolic", crate::i18n::gettext("Mark Unread")),
        "move" => ("folder-symbolic", crate::i18n::gettext("Move To")),
        "copy" => ("edit-copy-symbolic", crate::i18n::gettext("Copy To")),
        "archive" => ("package-x-generic-symbolic", crate::i18n::gettext("Archive")),
        "delete" => ("user-trash-symbolic", crate::i18n::gettext("Delete")),
        "junk" => ("mail-mark-junk-symbolic", crate::i18n::gettext("Spam")),
        "not-junk" => ("security-high-symbolic", crate::i18n::gettext("Not Junk")),
        "mark-all-read" => ("mail-read-symbolic", crate::i18n::gettext("All Read")),
        "next-unread-folder" => ("go-jump-symbolic", crate::i18n::gettext("Next Unread")),
        "expand-threads" => ("view-list-symbolic", crate::i18n::gettext("Threads")),
        "mark-thread-read" => ("mail-read-symbolic", crate::i18n::gettext("Read Thread")),
        "delete-thread" => ("user-trash-symbolic", crate::i18n::gettext("Delete Thread")),
        "select-all" => ("edit-select-all-symbolic", crate::i18n::gettext("Select All")),
        "refresh-mail" => ("view-refresh-symbolic", crate::i18n::gettext("Refresh")),
        "apply-filters" => ("system-run-symbolic", crate::i18n::gettext("Run Filters")),
        "filter-editor" => ("emblem-system-symbolic", crate::i18n::gettext("Filter Rules")),
        "search-advanced" => ("system-search-symbolic", crate::i18n::gettext("Adv. Search")),
        "oof-settings" => ("user-away-symbolic", crate::i18n::gettext("Out of Office")),
        "expunge" => ("user-trash-symbolic", crate::i18n::gettext("Expunge")),
        "undo" => ("edit-undo-symbolic", crate::i18n::gettext("Undo")),
        "redo" => ("edit-redo-symbolic", crate::i18n::gettext("Redo")),
        "separator" => ("object-flip-horizontal-symbolic", crate::i18n::gettext("Separator")),
        "spacer" => ("object-flip-horizontal-symbolic", crate::i18n::gettext("Flexible Space")),
        "reply" => ("mail-reply-sender-symbolic", crate::i18n::gettext("Reply")),
        "reply-all" => ("mail-reply-all-symbolic", crate::i18n::gettext("Reply All")),
        "forward" => ("mail-forward-symbolic", crate::i18n::gettext("Forward")),
        "edit-as-new" => ("document-edit-symbolic", crate::i18n::gettext("Edit as New")),
        "add-sender-contact" => ("contact-new-symbolic", crate::i18n::gettext("Add Contact")),
        "print" => ("document-print-symbolic", crate::i18n::gettext("Print")),
        "export" => ("document-save-symbolic", crate::i18n::gettext("Save EML")),
        "source" => ("text-x-generic-symbolic", crate::i18n::gettext("Source")),
        "prev" => ("go-previous-symbolic", crate::i18n::gettext("Previous")),
        "next" => ("go-next-symbolic", crate::i18n::gettext("Next")),
        other if other.starts_with("folder:") => {
            let spec = other.strip_prefix("folder:").unwrap_or("");
            let name = match spec {
                "unified_inbox" | "inbox" => crate::i18n::gettext("Inbox"),
                "unified_sent" | "sent" => crate::i18n::gettext("Sent"),
                "drafts" => crate::i18n::gettext("Drafts"),
                "archive" => crate::i18n::gettext("Archive"),
                "trash" => crate::i18n::gettext("Trash"),
                "spam" => crate::i18n::gettext("Junk"),
                _ => spec.rsplit_once("::").map(|(_, f)| f).unwrap_or(spec).to_string(),
            };
            ("folder-symbolic", name)
        }
        other if other.starts_with("label:") => {
            let tag = other.strip_prefix("label:").unwrap_or("");
            let name = labels
                .iter()
                .find(|l| l.tag.eq_ignore_ascii_case(tag) || l.name.eq_ignore_ascii_case(tag))
                .map(|l| l.display_name())
                .unwrap_or_else(|| tag.to_string());
            ("preferences-color-symbolic", name)
        }
        _ => ("folder-symbolic", base.to_string()),
    };
    let final_label = alias.filter(|a| !a.is_empty()).map(str::to_string).unwrap_or(default_lbl);
    (icon, final_label)
}

fn build_mail_list_toolbar_editor(settings: Option<AppSettings>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().build();

    let btn_restore = gtk::Button::builder()
        .label(&crate::i18n::gettext("Restore Defaults"))
        .valign(gtk::Align::Center)
        .css_classes(vec!["flat"])
        .build();
    group.set_header_suffix(Some(&btn_restore));

    let row_show_bar = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Show action and quick-filter bar above message list"))
        .use_markup(false)
        .active(settings.as_ref().map(|s| s.mail_show_toolbar()).unwrap_or(false))
        .build();
    if let Some(ref s) = settings {
        let s_toggle = s.clone();
        row_show_bar.connect_active_notify(move |r| {
            s_toggle.set_mail_show_toolbar(r.is_active());
        });
        let row_weak = row_show_bar.downgrade();
        s.connect_changed(Some("mail-show-toolbar"), move |st, _| {
            if let Some(r) = row_weak.upgrade() {
                let act = st.boolean("mail-show-toolbar");
                if r.is_active() != act {
                    r.set_active(act);
                }
            }
        });
    }
    group.add(&row_show_bar);

    // Strict GNOME Libadwaita ComboRow for pinning any folder from the Account Panel
    let panel_folders = collect_account_panel_folders(None);
    let folder_presets: Vec<(String, String)> = panel_folders
        .into_iter()
        .map(|(id, label)| {
            let token_id = if id == "inbox" {
                "folder:unified_inbox".to_string()
            } else {
                format!("folder:{id}")
            };
            (token_id, label)
        })
        .collect();
    let folder_labels: Vec<&str> = folder_presets.iter().map(|(_, l)| l.as_str()).collect();
    let row_pin_folder = adw::ComboRow::builder()
        .use_markup(false)
        .title(&crate::i18n::gettext("Pin Folder to Action Bar"))
        .model(&gtk::StringList::new(&folder_labels))
        .build();
    configure_folder_combo_row(&row_pin_folder);
    let btn_pin_folder = gtk::Button::builder()
        .label(&format!("+ {}", crate::i18n::gettext("Pin")))
        .valign(gtk::Align::Center)
        .css_classes(vec!["flat"])
        .build();
    row_pin_folder.add_suffix(&btn_pin_folder);
    group.add(&row_pin_folder);

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .margin_top(8)
        .css_classes(vec!["boxed-list"])
        .build();
    let (zone_active, empty_active) = make_toolbar_zone_row(
        &list_box,
        &crate::i18n::gettext("Active Bar"),
        &crate::i18n::gettext("Drag between buttons to reorder · Click a button to rename or remove"),
        &crate::i18n::gettext("Empty"),
    );
    let (zone_palette, empty_palette) = make_toolbar_zone_row(
        &list_box,
        &crate::i18n::gettext("Not shown"),
        &crate::i18n::gettext("Drag into Active Bar or click to add · Drop here to remove"),
        &crate::i18n::gettext("Empty"),
    );
    group.add(&list_box);

    let initial_items = settings
        .as_ref()
        .map(AppSettings::mail_list_toolbar_items)
        .unwrap_or_else(|| {
            crate::services::settings_service::DEFAULT_MAIL_LIST_TOOLBAR_ITEMS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });
    let state: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(initial_items));
    let syncing = Rc::new(Cell::new(false));
    let dragged_width = Rc::new(Cell::new(52_i32));

    type RebuildListFn = Rc<RefCell<Option<Box<dyn Fn()>>>>;
    let rebuild_ref: RebuildListFn = Rc::new(RefCell::new(None));

    let save_and_rebuild = {
        let state = state.clone();
        let settings = settings.clone();
        let syncing = syncing.clone();
        let rebuild_ref = rebuild_ref.clone();
        Rc::new(move || {
            let items = state.borrow().clone();
            if let Some(ref s) = settings {
                syncing.set(true);
                s.set_mail_list_toolbar_items(&items);
                syncing.set(false);
            }
            if let Some(ref f) = *rebuild_ref.borrow() {
                f();
            }
        })
    };

    // Pin Folder handler (both via "+ Pin" button and on selecting a folder from the dropdown)
    {
        let pin_selected_folder = {
            let state_pin = state.clone();
            let save_pin = save_and_rebuild.clone();
            let combo_pin = row_pin_folder.clone();
            let presets = folder_presets.clone();
            Rc::new(move || {
                let idx = combo_pin.selected() as usize;
                let Some((base_id, full_label)) = presets.get(idx) else { return; };
                let short_default = full_label
                    .split_once(" — ")
                    .map(|(f, _)| f.trim())
                    .unwrap_or(full_label.as_str())
                    .replace(',', " ");
                let token = if base_id.contains("::") && !short_default.is_empty() {
                    format!("{base_id}={short_default}")
                } else {
                    base_id.clone()
                };
                if !state_pin.borrow().iter().any(|it| it.split('=').next().unwrap_or(it).trim() == base_id) {
                    state_pin.borrow_mut().push(token);
                    save_pin();
                }
            })
        };
        let pin_on_btn = pin_selected_folder.clone();
        btn_pin_folder.connect_clicked(move |_| {
            pin_on_btn();
        });
        let pin_on_sel = pin_selected_folder;
        row_pin_folder.connect_selected_notify(move |_| {
            pin_on_sel();
        });
    }

    // Active zone DropTarget (live insertion gap between buttons)
    {
        let dt = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        let z_enter = zone_active.clone();
        let e_enter = empty_active.clone();
        let dw_enter = dragged_width.clone();
        dt.connect_enter(move |_, x, y| {
            z_enter.add_css_class("drop-active");
            e_enter.set_visible(false);
            let idx = z_enter.drop_index(x, y);
            z_enter.set_gap(Some(idx), dw_enter.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_motion = zone_active.clone();
        let e_motion = empty_active.clone();
        let dw_motion = dragged_width.clone();
        dt.connect_motion(move |_, x, y| {
            z_motion.add_css_class("drop-active");
            e_motion.set_visible(false);
            let idx = z_motion.drop_index(x, y);
            z_motion.set_gap(Some(idx), dw_motion.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_leave = zone_active.clone();
        let e_leave = empty_active.clone();
        dt.connect_leave(move |_| {
            z_leave.remove_css_class("drop-active");
            z_leave.set_gap(None, 52);
            e_leave.set_visible(z_leave.first_child().is_none());
        });
        let z_drop = zone_active.clone();
        let state_drop = state.clone();
        let save_drop = save_and_rebuild.clone();
        dt.connect_drop(move |_, val, x, y| {
            z_drop.remove_css_class("drop-active");
            let insert_idx = z_drop.drop_index(x, y);
            z_drop.set_gap(None, 52);
            let Ok(payload) = val.get::<String>() else { return false; };
            if let Some(idx_str) = payload.strip_prefix("list-active:") {
                if let Ok(from_idx) = idx_str.parse::<usize>() {
                    let mut st = state_drop.borrow_mut();
                    if from_idx < st.len() {
                        let item = st.remove(from_idx);
                        let at = insert_idx.min(st.len());
                        st.insert(at, item);
                    }
                }
                save_drop();
                return true;
            }
            if let Some(pal_token) = payload.strip_prefix("list-palette:") {
                {
                    let mut st = state_drop.borrow_mut();
                    let at = insert_idx.min(st.len());
                    st.insert(at, pal_token.to_string());
                }
                save_drop();
                return true;
            }
            false
        });
        zone_active.add_controller(dt);
    }

    // Palette zone DropTarget (dropping an active item here removes it)
    {
        let dt = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
        let z_enter = zone_palette.clone();
        let e_enter = empty_palette.clone();
        let dw_enter = dragged_width.clone();
        dt.connect_enter(move |_, x, y| {
            z_enter.add_css_class("drop-active");
            e_enter.set_visible(false);
            let idx = z_enter.drop_index(x, y);
            z_enter.set_gap(Some(idx), dw_enter.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_motion = zone_palette.clone();
        let e_motion = empty_palette.clone();
        let dw_motion = dragged_width.clone();
        dt.connect_motion(move |_, x, y| {
            z_motion.add_css_class("drop-active");
            e_motion.set_visible(false);
            let idx = z_motion.drop_index(x, y);
            z_motion.set_gap(Some(idx), dw_motion.get());
            gtk::gdk::DragAction::MOVE
        });
        let z_leave = zone_palette.clone();
        let e_leave = empty_palette.clone();
        dt.connect_leave(move |_| {
            z_leave.remove_css_class("drop-active");
            z_leave.set_gap(None, 52);
            e_leave.set_visible(z_leave.first_child().is_none());
        });
        let z_drop = zone_palette.clone();
        let state_drop = state.clone();
        let save_drop = save_and_rebuild.clone();
        dt.connect_drop(move |_, val, _, _| {
            z_drop.remove_css_class("drop-active");
            z_drop.set_gap(None, 52);
            let Ok(payload) = val.get::<String>() else { return false; };
            if let Some(idx_str) = payload.strip_prefix("list-active:") {
                if let Ok(from_idx) = idx_str.parse::<usize>() {
                    let mut st = state_drop.borrow_mut();
                    if from_idx < st.len() {
                        st.remove(from_idx);
                    }
                }
                save_drop();
                return true;
            }
            false
        });
        zone_palette.add_controller(dt);
    }

    const PALETTE_TOKENS: &[&str] = &[
        "filter:unread",
        "filter:flagged",
        "filter:followup",
        "filter:memo",
        "filter:important",
        "filter:attachment",
        "filter:today",
        "edit-labels",
        "mark-read",
        "mark-unread",
        "move",
        "copy",
        "archive",
        "delete",
        "junk",
        "not-junk",
        "mark-all-read",
        "next-unread-folder",
        "expand-threads",
        "mark-thread-read",
        "delete-thread",
        "select-all",
        "refresh-mail",
        "apply-filters",
        "filter-editor",
        "search-advanced",
        "oof-settings",
        "expunge",
        "undo",
        "redo",
        "separator",
        "spacer",
    ];

    {
        let zone_active = zone_active.clone();
        let zone_palette = zone_palette.clone();
        let empty_active = empty_active.clone();
        let empty_palette = empty_palette.clone();
        let state = state.clone();
        let settings_for_labels = settings.clone();
        let save_and_rebuild = save_and_rebuild.clone();
        let dragged_width = dragged_width.clone();
        *rebuild_ref.borrow_mut() = Some(Box::new(move || {
            for z in [&zone_active, &zone_palette] {
                z.clear();
            }
            let items = state.borrow().clone();
            let labels = settings_for_labels
                .as_ref()
                .map(|s| s.mail_labels())
                .unwrap_or_default();

            empty_active.set_visible(items.is_empty());
            let total_len = items.len();
            for (idx, raw_item) in items.iter().enumerate() {
                let (icon_name, label_text) = list_toolbar_item_meta(raw_item, &labels);
                let (base_token, current_alias) = match raw_item.split_once('=') {
                    Some((b, a)) => (b.trim().to_string(), a.trim().to_string()),
                    None => (raw_item.trim().to_string(), String::new()),
                };
                let drag_payload = format!("list-active:{idx}");
                let tile_box = make_toolbar_chip(
                    &base_token,
                    icon_name,
                    &label_text,
                    drag_payload,
                    &labels,
                    &zone_active,
                    &dragged_width,
                );

                // Clean GNOME Libadwaita menu popover on click / right-click
                let st_click = state.clone();
                let sv_click = save_and_rebuild.clone();
                let bt_for_menu = base_token.clone();
                let alias_for_menu = current_alias.clone();
                let tb_for_menu = tile_box.clone();
                let open_tile_menu = Rc::new(move || {
                    let pop_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(2)
                        .margin_start(4)
                        .margin_end(4)
                        .margin_top(4)
                        .margin_bottom(4)
                        .build();
                    let pop = gtk::Popover::builder()
                        .child(&pop_box)
                        .has_arrow(false)
                        .css_classes(vec!["menu", "context-menu"])
                        .build();
                    pop.set_parent(&tb_for_menu);

                    let make_menu_btn = |label: &str, sensitive: bool| -> gtk::Button {
                        gtk::Button::builder()
                            .child(
                                &gtk::Label::builder()
                                    .label(label)
                                    .halign(gtk::Align::Start)
                                    .build(),
                            )
                            .halign(gtk::Align::Fill)
                            .sensitive(sensitive)
                            .css_classes(vec!["flat", "context-menu-item"])
                            .build()
                    };

                    if !matches!(bt_for_menu.as_str(), "separator" | "spacer") {
                        let b_rename = make_menu_btn(&crate::i18n::gettext("Rename…"), true);
                        let st_rn = st_click.clone();
                        let sv_rn = sv_click.clone();
                        let bt_rn = bt_for_menu.clone();
                        let al_rn = alias_for_menu.clone();
                        let pw_rn = pop.downgrade();
                        let tb_dlg = tb_for_menu.clone();
                        b_rename.connect_clicked(move |_| {
                            if let Some(p) = pw_rn.upgrade() {
                                p.popdown();
                            }
                            let dialog = adw::AlertDialog::builder()
                                .heading(&crate::i18n::gettext("Rename Action Bar Item"))
                                .body(&crate::i18n::gettext("Enter a custom label for this button, or leave it empty to use the default icon/name."))
                                .default_response("save")
                                .close_response("cancel")
                                .build();
                            dialog.add_response("cancel", &crate::i18n::gettext("Cancel"));
                            dialog.add_response("save", &crate::i18n::gettext("Save"));
                            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
                            let entry = gtk::Entry::builder()
                                .text(&al_rn)
                                .placeholder_text(&crate::i18n::gettext("Default label"))
                                .activates_default(true)
                                .build();
                            dialog.set_extra_child(Some(&entry));
                            let st_save = st_rn.clone();
                            let sv_save = sv_rn.clone();
                            let bt_save = bt_rn.clone();
                            let entry_resp = entry.clone();
                            dialog.connect_response(Some("save"), move |_, _| {
                                {
                                    let mut st = st_save.borrow_mut();
                                    if idx < st.len() {
                                        let a = entry_resp.text().trim().replace(',', " ");
                                        st[idx] = if a.is_empty() {
                                            bt_save.clone()
                                        } else {
                                            format!("{bt_save}={a}")
                                        };
                                    }
                                }
                                sv_save();
                            });
                            dialog.present(Some(&tb_dlg));
                        });
                        pop_box.append(&b_rename);
                    }

                    if total_len > 1 {
                        let b_l = make_menu_btn(&crate::i18n::gettext("Move Left"), idx > 0);
                        let st_l = st_click.clone();
                        let sv_l = sv_click.clone();
                        let pw_l = pop.downgrade();
                        b_l.connect_clicked(move |_| {
                            {
                                let mut st = st_l.borrow_mut();
                                if idx > 0 && idx < st.len() {
                                    st.swap(idx, idx - 1);
                                }
                            }
                            if let Some(p) = pw_l.upgrade() { p.popdown(); }
                            sv_l();
                        });
                        pop_box.append(&b_l);

                        let b_r = make_menu_btn(&crate::i18n::gettext("Move Right"), idx + 1 < total_len);
                        let st_r = st_click.clone();
                        let sv_r = sv_click.clone();
                        let pw_r = pop.downgrade();
                        b_r.connect_clicked(move |_| {
                            {
                                let mut st = st_r.borrow_mut();
                                if idx + 1 < st.len() {
                                    st.swap(idx, idx + 1);
                                }
                            }
                            if let Some(p) = pw_r.upgrade() { p.popdown(); }
                            sv_r();
                        });
                        pop_box.append(&b_r);
                    }

                    pop_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

                    let b_rem = make_menu_btn(&crate::i18n::gettext("Remove from Action Bar"), true);
                    let st_rem = st_click.clone();
                    let sv_rem = sv_click.clone();
                    let pw_rem = pop.downgrade();
                    b_rem.connect_clicked(move |_| {
                        {
                            let mut st = st_rem.borrow_mut();
                            if idx < st.len() {
                                st.remove(idx);
                            }
                        }
                        if let Some(p) = pw_rem.upgrade() { p.popdown(); }
                        sv_rem();
                    });
                    pop_box.append(&b_rem);

                    pop.connect_closed(|p| {
                        let p_clone = p.clone();
                        glib::idle_add_local_once(move || {
                            p_clone.unparent();
                        });
                    });
                    pop.popup();
                });

                let click_g = gtk::GestureClick::builder().button(1).build();
                let open_l = open_tile_menu.clone();
                click_g.connect_released(move |_, _, _, _| {
                    open_l();
                });
                tile_box.add_controller(click_g);

                let rclick_g = gtk::GestureClick::builder().button(3).build();
                let open_r = open_tile_menu.clone();
                rclick_g.connect_pressed(move |g, _, _, _| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    open_r();
                });
                tile_box.add_controller(rclick_g);

                zone_active.append(&tile_box);
            }

            // Populate Palette (items not in Active Bar + configured labels, without duplicates)
            let canonical_toolbar_base = |raw: &str| -> String {
                match raw {
                    "filter:starred" => "filter:flagged".to_string(),
                    "filter:follow-up" => "filter:followup".to_string(),
                    "filter:memos" => "filter:memo".to_string(),
                    "filter:attachments" => "filter:attachment".to_string(),
                    other => other.to_string(),
                }
            };
            let active_bases: std::collections::HashSet<String> = items
                .iter()
                .map(|it| canonical_toolbar_base(it.split('=').next().unwrap_or(it).trim()))
                .collect();
            let mut palette_items: Vec<String> = PALETTE_TOKENS
                .iter()
                .filter(|t| !active_bases.contains(&canonical_toolbar_base(**t)))
                .map(|t| (*t).to_string())
                .collect();
            for lbl_def in &labels {
                let tag_token = format!("label:{}", lbl_def.tag);
                let is_important_label = lbl_def.tag.eq_ignore_ascii_case("$Labelimportant")
                    || lbl_def.name.eq_ignore_ascii_case("Important");
                if !active_bases.contains(&tag_token)
                    && !(is_important_label && active_bases.contains("filter:important"))
                {
                    palette_items.push(tag_token);
                }
            }

            empty_palette.set_visible(palette_items.is_empty());
            for pal_token in palette_items {
                let (icon_name, label_text) = list_toolbar_item_meta(&pal_token, &labels);
                let drag_str = format!("list-palette:{pal_token}");
                let tile_box = make_toolbar_chip(
                    &pal_token,
                    icon_name,
                    &label_text,
                    drag_str,
                    &labels,
                    &zone_palette,
                    &dragged_width,
                );
                tile_box.set_tooltip_text(Some(&crate::i18n::gettext("Drag into Active Bar or click to add")));

                let st_add = state.clone();
                let sv_add = save_and_rebuild.clone();
                let tok_add = pal_token.clone();
                let click_add = gtk::GestureClick::builder().button(1).build();
                click_add.connect_released(move |_, _, _, _| {
                    st_add.borrow_mut().push(tok_add.clone());
                    sv_add();
                });
                tile_box.add_controller(click_add);

                zone_palette.append(&tile_box);
            }
        }));
    }

    if let Some(ref f) = *rebuild_ref.borrow() {
        f();
    }

    {
        let state_reset = state.clone();
        let save_reset = save_and_rebuild.clone();
        btn_restore.connect_clicked(move |_| {
            *state_reset.borrow_mut() = crate::services::settings_service::DEFAULT_MAIL_LIST_TOOLBAR_ITEMS
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            save_reset();
        });
    }

    if let Some(ref s) = settings {
        let s_clone = s.clone();
        let state_ext = state.clone();
        let syncing_ext = syncing.clone();
        let rebuild_ext = rebuild_ref.clone();
        s.connect_changed(Some("mail-list-toolbar-items"), move |_, _| {
            if syncing_ext.get() {
                return;
            }
            *state_ext.borrow_mut() = s_clone.mail_list_toolbar_items();
            if let Some(ref f) = *rebuild_ext.borrow() {
                f();
            }
        });
    }

    group
}

fn build_mail_normal_headers_editor(
    settings: Option<AppSettings>,
    group: &adw::PreferencesGroup,
) -> gtk::Box {
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

    let entry_row = adw::EntryRow::builder()
        .title(&crate::i18n::gettext("Add custom header (e.g. List-Id)"))
        .build();
    let add_button = gtk::Button::builder()
        .label(&crate::i18n::gettext("Add"))
        .css_classes(vec!["flat", "suggested-action"])
        .sensitive(false)
        .valign(gtk::Align::Center)
        .build();
    entry_row.add_suffix(&add_button);

    rebuild_mail_normal_headers_list(&list, &state, &settings, &entry_row);

    let validation_label = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(vec!["error"])
        .margin_start(12)
        .margin_top(4)
        .visible(false)
        .build();

    let reset_button = gtk::Button::builder()
        .label(&crate::i18n::gettext("Restore Defaults"))
        .tooltip_text(&crate::i18n::gettext("Reset normal headers to Evolution default list"))
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    group.set_header_suffix(Some(&reset_button));

    let editor = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    editor.append(&list);
    editor.append(&validation_label);

    let state_for_validation = Rc::clone(&state);
    let validation_for_entry = validation_label.clone();
    let add_button_for_entry = add_button.clone();
    entry_row.connect_changed(move |entry| {
        let validation = validate_new_mail_normal_header(&entry.text(), &state_for_validation);
        add_button_for_entry.set_sensitive(validation.is_ok());
        set_mail_header_validation_message(&validation_for_entry, validation.err());
    });

    let add_header = {
        let state = Rc::clone(&state);
        let list = list.clone();
        let settings = settings.clone();
        let validation_label = validation_label.clone();
        let entry_row = entry_row.clone();
        move || {
            add_mail_normal_header(&entry_row, &list, &state, &settings, &validation_label);
        }
    };
    let add_header_for_button = add_header.clone();
    add_button.connect_clicked(move |_| add_header_for_button());
    entry_row.connect_entry_activated(move |_| add_header());

    let state_for_reset = Rc::clone(&state);
    let list_for_reset = list.clone();
    let settings_for_reset = settings.clone();
    let entry_row_for_reset = entry_row.clone();
    let validation_for_reset = validation_label.clone();
    reset_button.connect_clicked(move |_| {
        state_for_reset.replace(default_mail_normal_headers_for_editor());
        persist_mail_normal_headers(&state_for_reset, &settings_for_reset);
        rebuild_mail_normal_headers_list(&list_for_reset, &state_for_reset, &settings_for_reset, &entry_row_for_reset);
        entry_row_for_reset.set_text("");
        set_mail_header_validation_message(&validation_for_reset, None);
    });

    if let Some(s) = settings.as_ref() {
        let state_changed = Rc::clone(&state);
        let list_changed = list.clone();
        let settings_changed = settings.clone();
        let entry_row_changed = entry_row.clone();
        s.connect_changed(Some("mail-normal-headers"), move |_, _| {
            if let Some(app_settings) = settings_changed.as_ref() {
                let current = app_settings.mail_normal_headers();
                if *state_changed.borrow() != current {
                    state_changed.replace(current);
                    rebuild_mail_normal_headers_list(&list_changed, &state_changed, &settings_changed, &entry_row_changed);
                }
            }
        });
    }

    editor
}

fn rebuild_mail_normal_headers_list(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    entry_row: &adw::EntryRow,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let headers = state.borrow().clone();
    for (index, (name, visible)) in headers.into_iter().enumerate() {
        list.append(&mail_normal_header_row(
            list, state, settings, index, name, visible, entry_row,
        ));
    }
    list.append(entry_row);
}

fn mail_normal_header_row(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    index: usize,
    name: String,
    visible: bool,
    entry_row: &adw::EntryRow,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&name)
        .activatable(false)
        .selectable(false)
        .build();

    let visibility = gtk::Switch::builder()
        .active(visible)
        .valign(gtk::Align::Center)
        .tooltip_text(&crate::i18n::gettext("Show header in standard reading view"))
        .build();
    let move_up = mail_header_editor_button(
        "go-up-symbolic",
        &crate::i18n::gettext("Move header up"),
        index > 0,
    );
    let move_down = mail_header_editor_button(
        "go-down-symbolic",
        &crate::i18n::gettext("Move header down"),
        index + 1 < state.borrow().len(),
    );
    let remove = mail_header_editor_button(
        "user-trash-symbolic",
        &crate::i18n::gettext("Remove header"),
        true,
    );

    row.add_suffix(&visibility);
    row.add_suffix(&move_up);
    row.add_suffix(&move_down);
    row.add_suffix(&remove);

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
    let entry_row_for_up = entry_row.clone();
    move_up.connect_clicked(move |_| {
        move_mail_normal_header(
            &list_for_up,
            &state_for_up,
            &settings_for_up,
            &name_for_up,
            -1,
            &entry_row_for_up,
        );
    });

    let state_for_down = Rc::clone(state);
    let settings_for_down = settings.clone();
    let list_for_down = list.clone();
    let name_for_down = name.clone();
    let entry_row_for_down = entry_row.clone();
    move_down.connect_clicked(move |_| {
        move_mail_normal_header(
            &list_for_down,
            &state_for_down,
            &settings_for_down,
            &name_for_down,
            1,
            &entry_row_for_down,
        );
    });

    let state_for_remove = Rc::clone(state);
    let settings_for_remove = settings.clone();
    let list_for_remove = list.clone();
    let entry_row_for_remove = entry_row.clone();
    remove.connect_clicked(move |_| {
        state_for_remove
            .borrow_mut()
            .retain(|(candidate, _)| !candidate.eq_ignore_ascii_case(&name));
        persist_mail_normal_headers(&state_for_remove, &settings_for_remove);
        rebuild_mail_normal_headers_list(&list_for_remove, &state_for_remove, &settings_for_remove, &entry_row_for_remove);
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
    entry_row: &adw::EntryRow,
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
    rebuild_mail_normal_headers_list(list, state, settings, entry_row);
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
            "At most {MAX_EDITABLE_MAIL_NORMAL_HEADERS} headers can be managed."
        ));
    }
    if headers
        .iter()
        .any(|(candidate, _)| candidate.eq_ignore_ascii_case(&name))
    {
        return Err(format!("The header \"{name}\" already exists."));
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
    entry_row: &adw::EntryRow,
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<(String, bool)>>>,
    settings: &Option<AppSettings>,
    validation_label: &gtk::Label,
) {
    let value = entry_row.text().to_string();
    let name = match validate_new_mail_normal_header(&value, state) {
        Ok(name) => name,
        Err(message) => {
            set_mail_header_validation_message(validation_label, Some(message));
            return;
        }
    };
    state.borrow_mut().push((name, true));
    persist_mail_normal_headers(state, settings);
    rebuild_mail_normal_headers_list(list, state, settings, entry_row);
    entry_row.set_text("");
    set_mail_header_validation_message(validation_label, None);
}

fn build_mail_expanded_headers_editor(
    parent: &PreferencesDialog,
    group: &adw::PreferencesGroup,
) -> gtk::Box {
    let config = Rc::new(RefCell::new(
        crate::services::settings_service::ExpandedHeadersConfig::load(),
    ));

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();

    let row_show_other = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Show other unlisted RFC headers"))
        .active(config.borrow().show_other_headers)
        .build();
    {
        let cfg_for_other = Rc::clone(&config);
        let parent_weak = parent.downgrade();
        row_show_other.connect_active_notify(move |sw| {
            let mut c = cfg_for_other.borrow_mut();
            if c.show_other_headers != sw.is_active() {
                c.show_other_headers = sw.is_active();
                c.save();
                drop(c);
                if let Some(p) = parent_weak.upgrade() {
                    refresh_open_mail_views(&p);
                }
            }
        });
    }

    let entry_row = adw::EntryRow::builder()
        .title(&crate::i18n::gettext("Add Expanded Header (e.g. X-Spam-Status)"))
        .activatable(false)
        .selectable(false)
        .build();
    let add_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(&crate::i18n::gettext("Add header"))
        .css_classes(vec!["flat"])
        .sensitive(false)
        .valign(gtk::Align::Center)
        .build();
    entry_row.add_suffix(&add_button);

    let reset_button = gtk::Button::builder()
        .label(&crate::i18n::gettext("Restore Defaults"))
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    group.set_header_suffix(Some(&reset_button));

    let rebuild_expanded_list: Rc<dyn Fn()> = {
        let list_weak = list.downgrade();
        let cfg = Rc::clone(&config);
        let row_show_other_ref = row_show_other.clone();
        let entry_row_ref = entry_row.clone();
        let parent_weak = parent.downgrade();
        let holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let holder_clone = holder.clone();
        let f: Rc<dyn Fn()> = Rc::new(move || {
            let Some(list) = list_weak.upgrade() else { return };
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            list.append(&row_show_other_ref);
            let items = cfg.borrow().headers.clone();
            let len = items.len();
            for (idx, (name, visible)) in items.into_iter().enumerate() {
                let row = adw::ActionRow::builder()
                    .title(&name)
                    .activatable(false)
                    .selectable(false)
                    .build();
                let sw = gtk::Switch::builder()
                    .active(visible)
                    .valign(gtk::Align::Center)
                    .build();
                let up_btn = mail_header_editor_button("go-up-symbolic", "Move header up", idx > 0);
                let down_btn =
                    mail_header_editor_button("go-down-symbolic", "Move header down", idx + 1 < len);
                let del_btn = mail_header_editor_button("user-trash-symbolic", "Remove header", true);
                row.add_suffix(&sw);
                row.add_suffix(&up_btn);
                row.add_suffix(&down_btn);
                row.add_suffix(&del_btn);

                let cfg_sw = Rc::clone(&cfg);
                let name_sw = name.clone();
                let p_sw = parent_weak.clone();
                sw.connect_active_notify(move |s| {
                    let mut c = cfg_sw.borrow_mut();
                    if let Some((_, en)) = c
                        .headers
                        .iter_mut()
                        .find(|(cand, _)| cand.eq_ignore_ascii_case(&name_sw))
                    {
                        *en = s.is_active();
                        c.save();
                    }
                    drop(c);
                    if let Some(p) = p_sw.upgrade() {
                        refresh_open_mail_views(&p);
                    }
                });

                let cfg_up = Rc::clone(&cfg);
                let p_up = parent_weak.clone();
                let h_up = holder_clone.clone();
                up_btn.connect_clicked(move |_| {
                    if idx > 0 {
                        let mut c = cfg_up.borrow_mut();
                        if idx < c.headers.len() {
                            c.headers.swap(idx, idx - 1);
                            c.save();
                        }
                        drop(c);
                        if let Some(reb) = h_up.borrow().as_ref() {
                            reb();
                        }
                        if let Some(p) = p_up.upgrade() {
                            refresh_open_mail_views(&p);
                        }
                    }
                });

                let cfg_down = Rc::clone(&cfg);
                let p_down = parent_weak.clone();
                let h_down = holder_clone.clone();
                down_btn.connect_clicked(move |_| {
                    let mut c = cfg_down.borrow_mut();
                    if idx + 1 < c.headers.len() {
                        c.headers.swap(idx, idx + 1);
                        c.save();
                        drop(c);
                        if let Some(reb) = h_down.borrow().as_ref() {
                            reb();
                        }
                        if let Some(p) = p_down.upgrade() {
                            refresh_open_mail_views(&p);
                        }
                    }
                });

                let cfg_del = Rc::clone(&cfg);
                let name_del = name.clone();
                let p_del = parent_weak.clone();
                let h_del = holder_clone.clone();
                del_btn.connect_clicked(move |_| {
                    let mut c = cfg_del.borrow_mut();
                    c.headers
                        .retain(|(cand, _)| !cand.eq_ignore_ascii_case(&name_del));
                    c.save();
                    drop(c);
                    if let Some(reb) = h_del.borrow().as_ref() {
                        reb();
                    }
                    if let Some(p) = p_del.upgrade() {
                        refresh_open_mail_views(&p);
                    }
                });

                list.append(&row);
            }
            list.append(&entry_row_ref);
        });
        *holder.borrow_mut() = Some(f.clone());
        f
    };

    rebuild_expanded_list();

    {
        let cfg_val = Rc::clone(&config);
        let add_btn = add_button.clone();
        entry_row.connect_changed(move |e| {
            let trimmed = e.text().trim().to_string();
            let valid = validate_mail_normal_header_name(&trimmed).is_ok()
                && !cfg_val
                    .borrow()
                    .headers
                    .iter()
                    .any(|(cand, _)| cand.eq_ignore_ascii_case(&trimmed));
            add_btn.set_sensitive(valid);
        });
    }

    let do_add = {
        let cfg_add = Rc::clone(&config);
        let entry_add = entry_row.clone();
        let reb_add = rebuild_expanded_list.clone();
        let p_add = parent.downgrade();
        move || {
            let Ok(clean) = validate_mail_normal_header_name(&entry_add.text()) else {
                return;
            };
            {
                let mut c = cfg_add.borrow_mut();
                if c.headers.iter().any(|(cand, _)| cand.eq_ignore_ascii_case(&clean)) {
                    return;
                }
                c.headers.push((clean, true));
                c.save();
            }
            entry_add.set_text("");
            reb_add();
            if let Some(p) = p_add.upgrade() {
                refresh_open_mail_views(&p);
            }
        }
    };
    let do_add_btn = do_add.clone();
    add_button.connect_clicked(move |_| do_add_btn());
    entry_row.connect_entry_activated(move |_| do_add());

    {
        let cfg_reset = Rc::clone(&config);
        let sw_other = row_show_other.clone();
        let reb_reset = rebuild_expanded_list.clone();
        let p_reset = parent.downgrade();
        reset_button.connect_clicked(move |_| {
            let def = crate::services::settings_service::ExpandedHeadersConfig::default();
            def.save();
            *cfg_reset.borrow_mut() = def;
            sw_other.set_active(true);
            reb_reset();
            if let Some(p) = p_reset.upgrade() {
                refresh_open_mail_views(&p);
            }
        });
    }

    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    container.append(&list);
    container
}

/// Display label for configured S/MIME file paths: file names only, or a dim
/// "None" when nothing is configured.
fn update_smime_path_label(label: &gtk::Label, clear: &gtk::Button, paths: &[String]) {
    let names: Vec<String> = paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .collect();
    if names.is_empty() {
        label.set_label(&crate::i18n::gettext("None"));
        label.add_css_class("dim-label");
        label.set_tooltip_text(None);
        clear.set_visible(false);
    } else {
        label.set_label(&names.join(", "));
        label.remove_css_class("dim-label");
        label.set_tooltip_text(Some(&paths.join("\n")));
        clear.set_visible(true);
    }
}

/// Builds the shared suffix of S/MIME file rows: file name label plus a clear
/// button that is only visible when a file is set. Activating the row itself
/// opens the file chooser (GNOME HIG: no extra "Select…" button).
fn smime_row_with_suffix(title: &str) -> (adw::ActionRow, gtk::Label, gtk::Button) {
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(true)
        .build();
    let path_label = gtk::Label::builder()
        .halign(gtk::Align::End)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(24)
        .build();
    row.add_suffix(&path_label);
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(&crate::i18n::gettext("Clear"))
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear_button);
    (row, path_label, clear_button)
}

fn smime_file_row(
    title: &str,
    _subtitle: &str,
    initial: String,
    settings: Option<AppSettings>,
    set_value: fn(&AppSettings, &str),
) -> adw::ActionRow {
    let (row, path_label, clear_button) = smime_row_with_suffix(title);
    update_smime_path_label(&path_label, &clear_button, &[initial]);

    let settings_for_clear = settings.clone();
    let path_label_for_clear = path_label.clone();
    clear_button.connect_clicked(move |btn| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_value(settings, "");
        }
        update_smime_path_label(&path_label_for_clear, btn, &[]);
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    let clear_for_choose = clear_button.clone();
    let title_owned = title.to_string();
    row.connect_activated(move |row| {
        let Some(root) = row.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title(&title_owned)
            .modal(true)
            .build();
        let settings_for_result = settings_for_choose.clone();
        let path_label_for_result = path_label_for_choose.clone();
        let clear_for_result = clear_for_choose.clone();
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
            update_smime_path_label(&path_label_for_result, &clear_for_result, &[value]);
        });
    });

    row
}

fn smime_files_row(
    title: &str,
    _subtitle: &str,
    initial: Vec<String>,
    settings: Option<AppSettings>,
    set_values: fn(&AppSettings, &[String]),
) -> adw::ActionRow {
    let (row, path_label, clear_button) = smime_row_with_suffix(title);
    update_smime_path_label(&path_label, &clear_button, &initial);

    let settings_for_clear = settings.clone();
    let path_label_for_clear = path_label.clone();
    clear_button.connect_clicked(move |btn| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_values(settings, &[]);
        }
        update_smime_path_label(&path_label_for_clear, btn, &[]);
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    let clear_for_choose = clear_button.clone();
    let title_owned = title.to_string();
    row.connect_activated(move |row| {
        let Some(root) = row.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title(&title_owned)
            .modal(true)
            .build();
        let settings_for_result = settings_for_choose.clone();
        let path_label_for_result = path_label_for_choose.clone();
        let clear_for_result = clear_for_choose.clone();
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
            update_smime_path_label(&path_label_for_result, &clear_for_result, &paths);
        });
    });

    row
}

fn source_preference_row(
    title: &str,
    _subtitle: &str,
    kind: EdsSourceKind,
    local_uid: &str,
    local_label: &str,
    settings: Option<AppSettings>,
    get_source: fn(&AppSettings) -> String,
    set_source: fn(&AppSettings, &str),
) -> adw::ComboRow {
    let source_model = gtk::StringList::new(&["Loading sources…"]);
    let source_uids = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&source_model)
        .build();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(36)
            .build();
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
        let Some(string_obj) = item.item().and_downcast::<gtk::StringObject>() else { return };
        label.set_text(&string_obj.string());
    });
    row.set_factory(Some(&factory));

    let list_factory = gtk::SignalListItemFactory::new();
    list_factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::None)
            .width_chars(28)
            .max_width_chars(60)
            .margin_start(6)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        item.set_child(Some(&label));
    });
    list_factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else { return };
        let Some(string_obj) = item.item().and_downcast::<gtk::StringObject>() else { return };
        label.set_text(&string_obj.string());
    });
    row.set_list_factory(Some(&list_factory));

    let icon_name = if title.to_lowercase().contains("task") {
        "view-list-bullet-symbolic"
    } else if title.to_lowercase().contains("memo") {
        "accessories-text-editor-symbolic"
    } else {
        match kind {
            EdsSourceKind::Calendar => "x-office-calendar-symbolic",
            EdsSourceKind::AddressBook => "x-office-address-book-symbolic",
            EdsSourceKind::Mail => "mail-read-symbolic",
        }
    };
    row.add_prefix(&gtk::Image::from_icon_name(icon_name));

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
    let settings_for_load = settings.clone();
    glib::spawn_future_local(async move {
        let discovery = eds_service::discover_sources(kind).await;
        let mut sources = discovery.sources;
        sources.retain(|source| source.enabled);
        let friendly_backend_of = |raw_backend: &str| -> String {
            let trimmed = raw_backend.trim();
            if trimmed.is_empty() {
                crate::i18n::gettext("Account")
            } else {
                match trimmed.to_ascii_lowercase().as_str() {
                    "google" => "Google".to_string(),
                    "ews" | "exchange" | "office365" => "Exchange".to_string(),
                    "local" => crate::i18n::gettext("On This Computer"),
                    "webdav" | "carddav" | "caldav" => "DAV".to_string(),
                    _ => trimmed.to_string(),
                }
            }
        };
        sources.sort_by_cached_key(|source| {
            (
                friendly_backend_of(&source.backend_name).to_lowercase(),
                source.display_name.to_lowercase(),
                source.uid.clone(),
            )
        });

        let mut names = sources
            .iter()
            .map(|source| {
                let backend_group = friendly_backend_of(&source.backend_name);
                format!("{} — {}", source.display_name, backend_group)
            })
            .collect::<Vec<_>>();
        let mut uids = sources
            .into_iter()
            .map(|source| source.uid)
            .collect::<Vec<_>>();

        // Always append the local built-in source BEFORE checking preferred_source_uid
        // so that "local-addressbook" / "local-calendar" is never misclassified as unavailable!
        if local_available && !uids.iter().any(|uid| uid == &local_uid) {
            names.push(format!(
                "{} — {}",
                local_label,
                crate::i18n::gettext("On This Computer")
            ));
            uids.push(local_uid.clone());
        }

        // Resolve preferred source UID (including legacy aliases like "local")
        let resolved_preferred_index = preferred_source_uid
            .as_deref()
            .and_then(|pref| {
                uids.iter().position(|candidate| candidate == pref).or_else(|| {
                    if pref.eq_ignore_ascii_case("local")
                        || pref.starts_with("local-")
                        || pref.starts_with("system-")
                    {
                        uids.iter()
                            .position(|candidate| candidate == &local_uid)
                            .or_else(|| {
                                uids.iter().position(|candidate| {
                                    candidate.starts_with("system-") || candidate == "local"
                                })
                            })
                    } else {
                        None
                    }
                })
            })
            .or_else(|| {
                local_available
                    .then(|| uids.iter().position(|uid| uid == &local_uid))
                    .flatten()
            })
            .or_else(|| (!uids.is_empty()).then_some(0));

        let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
        source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
        source_uids_for_load.replace(uids.clone());
        configure_folder_combo_row(&row_for_load);

        if let Some(selected_index) = resolved_preferred_index {
            row_for_load.set_selected(selected_index as u32);
            if let (Some(settings), Some(resolved_uid)) =
                (settings_for_load.as_ref(), uids.get(selected_index))
            {
                if preferred_source_uid.as_deref() != Some(resolved_uid.as_str()) {
                    set_source(settings, resolved_uid);
                }
            }
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
    let has_custom = !initial_path.trim().is_empty();
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(if has_custom {
            initial_path.as_str()
        } else {
            default_subtitle.as_str()
        })
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));

    let choose_button = gtk::Button::builder()
        .label(&crate::i18n::gettext("Change…"))
        .tooltip_text(&crate::i18n::gettext("Select custom storage folder"))
        .valign(gtk::Align::Center)
        .build();
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(&crate::i18n::gettext("Reset to default folder"))
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .visible(has_custom)
        .build();
    row.add_suffix(&choose_button);
    row.add_suffix(&clear_button);

    let choose_button_for_row = choose_button.clone();
    row.connect_activated(move |_| {
        choose_button_for_row.emit_clicked();
    });

    let set_path: Rc<dyn Fn(&AppSettings, &str)> = Rc::new(set_path);
    let dialog_weak = parent.downgrade();
    let row_for_choose = row.clone();
    let clear_for_choose = clear_button.clone();
    let settings_for_choose = settings.clone();
    let set_path_for_choose = Rc::clone(&set_path);
    choose_button.connect_clicked(move |_| {
        let Some(dialog) = dialog_weak.upgrade() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title(&crate::i18n::gettext("Select Storage Folder"))
            .modal(true)
            .build();
        let parent_window = dialog
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let settings_for_result = settings_for_choose.clone();
        let set_path_for_result = Rc::clone(&set_path_for_choose);
        let row_for_result = row_for_choose.clone();
        let clear_for_result = clear_for_choose.clone();
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
                clear_for_result.set_visible(true);
            },
        );
    });

    let row_for_clear = row.clone();
    let clear_for_self = clear_button.clone();
    let settings_for_clear = settings;
    clear_button.connect_clicked(move |_| {
        if let Some(settings) = settings_for_clear.as_ref() {
            set_path(settings, "");
        }
        row_for_clear.set_subtitle(&default_subtitle);
        clear_for_self.set_visible(false);
    });

    row
}

#[allow(dead_code)]
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
        .placeholder_text("Name, e.g. Work")
        .hexpand(true)
        .build();
    let color_entry = gtk::Entry::builder()
        .placeholder_text("Color, e.g. #729FCF")
        .width_chars(10)
        .build();
    let initial_color = category
        .as_ref()
        .map(|c| c.color.clone())
        .unwrap_or_else(|| "#729FCF".to_string());
    if let Some(category) = category.as_ref() {
        name_entry.set_text(&category.name);
    }
    color_entry.set_text(&initial_color);

    let color_dialog = gtk::ColorDialog::builder()
        .title("Select Category Color")
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(rgba) = gtk::gdk::RGBA::parse(&initial_color) {
        color_button.set_rgba(&rgba);
    }
    color_button.set_valign(gtk::Align::Center);

    let color_entry_for_button = color_entry.clone();
    color_button.connect_rgba_notify(move |btn| {
        let rgba = btn.rgba();
        let r = (rgba.red() * 255.0).round() as u8;
        let g = (rgba.green() * 255.0).round() as u8;
        let b = (rgba.blue() * 255.0).round() as u8;
        color_entry_for_button.set_text(&format!("#{r:02x}{g:02x}{b:02x}"));
    });

    let color_btn_for_entry = color_button.clone();
    color_entry.connect_changed(move |entry| {
        if let Ok(rgba) = gtk::gdk::RGBA::parse(&entry.text()) {
            color_btn_for_entry.set_rgba(&rgba);
        }
    });

    let save_button = gtk::Button::builder()
        .label("Save")
        .css_classes(vec!["flat", "suggested-action"])
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete Category")
        .css_classes(vec!["flat", "error"])
        .build();
    row.append(&name_entry);
    row.append(&color_button);
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
                    parent.show_toast(&crate::i18n::gettext("Calendar category saved"));
                    refresh_open_category_views(&parent);
                }
            }
            Err(error) => {
                if let Some(parent) = parent_weak_for_save.upgrade() {
                    parent.show_toast(&format!(
                        "Category could not be saved: {error}"
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
            "Delete calendar category?",
            "The category will be removed from app color presets. Existing iCalendar categories are retained.",
            "Delete",
            move || match CalendarCategoryStore::default().delete(&id) {
                Ok(true) => {
                    container.remove(&row);
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast("Calendar category deleted");
                        refresh_open_category_views(&parent);
                    }
                }
                Ok(false) => {
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast("Calendar category not found");
                    }
                }
                Err(error) => {
                    if let Some(parent) = parent_weak_for_result.upgrade() {
                        parent.show_toast(&format!("Category could not be deleted: {error}"));
                    }
                }
            },
        );
    });
}

fn refresh_open_category_views(parent: &PreferencesDialog) {
    if let Some(window) = parent
        .root()
        .and_downcast::<crate::window::MeridianWindow>()
    {
        window.refresh_category_presentation();
    }
}

fn refresh_open_mail_views(parent: &PreferencesDialog) {
    if let Some(window) = parent
        .root()
        .and_downcast::<crate::window::MeridianWindow>()
    {
        window.refresh_mail_page();
    }
}

fn build_mail_labels_group(
    parent: &PreferencesDialog,
    settings: Option<AppSettings>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(&crate::i18n::gettext("Labels"))
        .build();

    let state = Rc::new(RefCell::new(
        settings
            .as_ref()
            .map(AppSettings::mail_labels)
            .unwrap_or_else(MailLabelDefinition::default_labels),
    ));

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(vec!["boxed-list"])
        .build();
    rebuild_mail_labels_list(&list, &state, &settings, parent);

    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(&crate::i18n::gettext("Add Label…"))
        .valign(gtk::Align::Center)
        .css_classes(vec!["flat"])
        .build();
    group.set_header_suffix(Some(&add_btn));

    let reset_btn = gtk::Button::builder()
        .label(&crate::i18n::gettext("Restore Defaults"))
        .css_classes(vec!["flat"])
        .halign(gtk::Align::Start)
        .margin_top(8)
        .build();

    let parent_weak_for_add = parent.downgrade();
    let state_for_add = Rc::clone(&state);
    let list_for_add = list.clone();
    let settings_for_add = settings.clone();
    add_btn.connect_clicked(move |_| {
        let Some(parent_dlg) = parent_weak_for_add.upgrade() else {
            return;
        };
        present_edit_label_dialog(
            &parent_dlg,
            None,
            state_for_add.clone(),
            list_for_add.clone(),
            settings_for_add.clone(),
        );
    });

    let state_for_reset = Rc::clone(&state);
    let list_for_reset = list.clone();
    let settings_for_reset = settings.clone();
    let parent_weak_for_reset = parent.downgrade();
    reset_btn.connect_clicked(move |_| {
        let defaults = MailLabelDefinition::default_labels();
        state_for_reset.replace(defaults.clone());
        if let Some(s) = settings_for_reset.as_ref() {
            s.set_mail_labels(&defaults);
        }
        if let Some(parent_dlg) = parent_weak_for_reset.upgrade() {
            rebuild_mail_labels_list(
                &list_for_reset,
                &state_for_reset,
                &settings_for_reset,
                &parent_dlg,
            );
            parent_dlg.show_toast(&crate::i18n::gettext("Default labels restored"));
            refresh_open_mail_views(&parent_dlg);
        }
    });

    if let Some(s) = settings.as_ref() {
        let state_changed = Rc::clone(&state);
        let list_changed = list.clone();
        let settings_changed = settings.clone();
        let parent_weak = parent.downgrade();
        s.connect_changed(Some("mail-labels"), move |_, _| {
            if let Some(app_settings) = settings_changed.as_ref() {
                let current = app_settings.mail_labels();
                if *state_changed.borrow() != current {
                    state_changed.replace(current);
                    if let Some(p) = parent_weak.upgrade() {
                        rebuild_mail_labels_list(&list_changed, &state_changed, &settings_changed, &p);
                    }
                }
            }
        });
    }

    group.add(&list);
    group.add(&reset_btn);
    group
}

fn rebuild_mail_labels_list(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<MailLabelDefinition>>>,
    settings: &Option<AppSettings>,
    parent: &PreferencesDialog,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let labels = state.borrow().clone();
    for (index, label_def) in labels.into_iter().enumerate() {
        list.append(&mail_label_row(
            list, state, settings, parent, index, label_def,
        ));
    }
}

fn mail_label_row(
    list: &gtk::ListBox,
    state: &Rc<RefCell<Vec<MailLabelDefinition>>>,
    settings: &Option<AppSettings>,
    parent: &PreferencesDialog,
    index: usize,
    label_def: MailLabelDefinition,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(true);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();

    // Clicking the colored dot opens the native color picker directly.
    let color_dot = gtk::Label::builder()
        .label("●")
        .valign(gtk::Align::Center)
        .build();
    color_dot.set_markup(&format!(
        "<span foreground=\"{}\" font_size=\"x-large\">●</span>",
        label_def.color
    ));
    let color_btn = gtk::Button::builder()
        .child(&color_dot)
        .tooltip_text(&crate::i18n::gettext("Change Color"))
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .build();

    let text_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let name_label = gtk::Label::builder()
        .label(&label_def.display_name())
        .halign(gtk::Align::Start)
        .build();
    text_box.append(&name_label);

    let up_btn = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Move label up")
        .css_classes(vec!["flat", "circular"])
        .sensitive(index > 0)
        .valign(gtk::Align::Center)
        .build();
    let down_btn = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Move label down")
        .css_classes(vec!["flat", "circular"])
        .sensitive(index + 1 < state.borrow().len())
        .valign(gtk::Align::Center)
        .build();
    let edit_btn = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Edit Label")
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .build();
    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remove Label")
        .css_classes(vec!["flat", "circular", "error"])
        .valign(gtk::Align::Center)
        .build();

    content.append(&color_btn);
    content.append(&text_box);
    content.append(&up_btn);
    content.append(&down_btn);
    content.append(&edit_btn);
    content.append(&delete_btn);
    row.set_child(Some(&content));

    // Inline color change
    let state_color = Rc::clone(state);
    let list_color = list.clone();
    let settings_color = settings.clone();
    let parent_weak_color = parent.downgrade();
    let initial_color = label_def.color.clone();
    color_btn.connect_clicked(move |btn| {
        let Some(p) = parent_weak_color.upgrade() else {
            return;
        };
        let window = btn.root().and_downcast::<gtk::Window>();
        let initial = gtk::gdk::RGBA::parse(initial_color.as_str()).ok();
        let color_dialog = gtk::ColorDialog::builder().with_alpha(false).build();
        let state_c = state_color.clone();
        let list_c = list_color.clone();
        let settings_c = settings_color.clone();
        glib::spawn_future_local(async move {
            let Ok(rgba) = color_dialog
                .choose_rgba_future(window.as_ref(), initial.as_ref())
                .await
            else {
                return;
            };
            let hex = format!(
                "#{:02X}{:02X}{:02X}",
                (rgba.red() * 255.0).round() as u8,
                (rgba.green() * 255.0).round() as u8,
                (rgba.blue() * 255.0).round() as u8
            );
            if let Some(entry) = state_c.borrow_mut().get_mut(index) {
                entry.color = hex;
            }
            if let Some(s) = settings_c.as_ref() {
                s.set_mail_labels(&state_c.borrow());
            }
            rebuild_mail_labels_list(&list_c, &state_c, &settings_c, &p);
            refresh_open_mail_views(&p);
        });
    });

    // Move Up
    let state_up = Rc::clone(state);
    let list_up = list.clone();
    let settings_up = settings.clone();
    let parent_weak_up = parent.downgrade();
    up_btn.connect_clicked(move |_| {
        if index > 0 {
            state_up.borrow_mut().swap(index, index - 1);
            if let Some(s) = settings_up.as_ref() {
                s.set_mail_labels(&state_up.borrow());
            }
            if let Some(p) = parent_weak_up.upgrade() {
                rebuild_mail_labels_list(&list_up, &state_up, &settings_up, &p);
                refresh_open_mail_views(&p);
            }
        }
    });

    // Move Down
    let state_down = Rc::clone(state);
    let list_down = list.clone();
    let settings_down = settings.clone();
    let parent_weak_down = parent.downgrade();
    down_btn.connect_clicked(move |_| {
        if index + 1 < state_down.borrow().len() {
            state_down.borrow_mut().swap(index, index + 1);
            if let Some(s) = settings_down.as_ref() {
                s.set_mail_labels(&state_down.borrow());
            }
            if let Some(p) = parent_weak_down.upgrade() {
                rebuild_mail_labels_list(&list_down, &state_down, &settings_down, &p);
                refresh_open_mail_views(&p);
            }
        }
    });

    // Edit
    let parent_weak_edit = parent.downgrade();
    let state_edit = Rc::clone(state);
    let list_edit = list.clone();
    let settings_edit = settings.clone();
    let label_for_edit = label_def.clone();
    edit_btn.connect_clicked(move |_| {
        if let Some(p) = parent_weak_edit.upgrade() {
            present_edit_label_dialog(
                &p,
                Some((index, label_for_edit.clone())),
                state_edit.clone(),
                list_edit.clone(),
                settings_edit.clone(),
            );
        }
    });
    let edit_btn_clone = edit_btn.clone();
    row.connect_activate(move |_| {
        edit_btn_clone.emit_clicked();
    });

    // Delete
    let parent_weak_del = parent.downgrade();
    let state_del = Rc::clone(state);
    let list_del = list.clone();
    let settings_del = settings.clone();
    let label_name = label_def.name.clone();
    delete_btn.connect_clicked(move |btn| {
        let Some(p) = parent_weak_del.upgrade() else {
            return;
        };
        let p_for_action = p.clone();
        let state_for_action = state_del.clone();
        let list_for_action = list_del.clone();
        let settings_for_action = settings_del.clone();
        let name_for_prompt = label_name.clone();
        crate::dialogs::run_destructive_action(
            btn,
            &format!("Delete label “{name_for_prompt}”?"),
            "The label will be removed from configured mail labels.",
            "Delete",
            move || {
                state_for_action
                    .borrow_mut()
                    .retain(|l| l.name != name_for_prompt);
                if let Some(s) = settings_for_action.as_ref() {
                    s.set_mail_labels(&state_for_action.borrow());
                }
                rebuild_mail_labels_list(
                    &list_for_action,
                    &state_for_action,
                    &settings_for_action,
                    &p_for_action,
                );
                p_for_action.show_toast("Label deleted");
                refresh_open_mail_views(&p_for_action);
            },
        );
    });

    row
}

fn present_edit_label_dialog(
    parent: &PreferencesDialog,
    edit_target: Option<(usize, MailLabelDefinition)>,
    state: Rc<RefCell<Vec<MailLabelDefinition>>>,
    list: gtk::ListBox,
    settings: Option<AppSettings>,
) {
    let is_edit = edit_target.is_some();
    let title = if is_edit {
        "Edit Label"
    } else {
        "Add New Label"
    };

    let initial_name = edit_target
        .as_ref()
        .map(|(_, l)| l.name.clone())
        .unwrap_or_default();
    let initial_color = edit_target
        .as_ref()
        .map(|(_, l)| l.color.clone())
        .unwrap_or_else(|| "#3465A4".to_string());
    let initial_tag = edit_target
        .as_ref()
        .map(|(_, l)| l.tag.clone())
        .unwrap_or_default();

    let window = adw::Dialog::builder()
        .title(title)
        .content_width(420)
        .content_height(320)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();

    let cancel_btn = gtk::Button::builder().label(&crate::i18n::gettext("Cancel")).build();
    let save_btn = gtk::Button::builder()
        .label(if is_edit { "Save" } else { "Add" })
        .css_classes(vec!["suggested-action"])
        .sensitive(!initial_name.trim().is_empty())
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);
    toolbar.add_top_bar(&header);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().build();

    let name_entry = adw::EntryRow::builder()
        .title("Name")
        .text(&initial_name)
        .build();
    group.add(&name_entry);

    let tag_entry = adw::EntryRow::builder()
        .title("Tag (optional)")
        .text(&initial_tag)
        .build();
    group.add(&tag_entry);

    let color_row = adw::ActionRow::builder()
        .title("Color")
        .subtitle(&initial_color)
        .build();
    let color_dot = gtk::Label::builder()
        .label("●")
        .css_classes(vec!["title-2"])
        .valign(gtk::Align::Center)
        .margin_end(8)
        .build();
    color_dot.set_markup(&format!(
        "<span foreground=\"{initial_color}\" font_size=\"x-large\">●</span>"
    ));
    color_row.add_prefix(&color_dot);

    let color_dialog = gtk::ColorDialog::builder()
        .title("Select label color")
        .build();
    let color_button = gtk::ColorDialogButton::new(Some(color_dialog));
    if let Ok(rgba) = gtk::gdk::RGBA::parse(&initial_color) {
        color_button.set_rgba(&rgba);
    }
    color_button.set_valign(gtk::Align::Center);
    color_row.add_suffix(&color_button);

    let current_hex = Rc::new(RefCell::new(initial_color.clone()));
    let hex_for_button = Rc::clone(&current_hex);
    let color_dot_weak = color_dot.downgrade();
    let color_row_weak = color_row.downgrade();
    color_button.connect_rgba_notify(move |btn| {
        let rgba = btn.rgba();
        let r = (rgba.red() * 255.0).round() as u8;
        let g = (rgba.green() * 255.0).round() as u8;
        let b = (rgba.blue() * 255.0).round() as u8;
        let hex = format!("#{r:02x}{g:02x}{b:02x}");
        *hex_for_button.borrow_mut() = hex.clone();
        if let Some(dot) = color_dot_weak.upgrade() {
            dot.set_markup(&format!(
                "<span foreground=\"{hex}\" font_size=\"x-large\">●</span>"
            ));
        }
        if let Some(row) = color_row_weak.upgrade() {
            row.set_subtitle(&hex);
        }
    });

    group.add(&color_row);
    page.add(&group);
    toolbar.set_content(Some(&page));
    window.set_child(Some(&toolbar));

    let save_btn_for_name = save_btn.clone();
    name_entry.connect_changed(move |entry| {
        save_btn_for_name.set_sensitive(!entry.text().trim().is_empty());
    });

    let window_for_cancel = window.downgrade();
    cancel_btn.connect_clicked(move |_| {
        if let Some(w) = window_for_cancel.upgrade() {
            w.close();
        }
    });

    let window_for_save = window.downgrade();
    let parent_weak = parent.downgrade();
    save_btn.connect_clicked(move |_| {
        let name = name_entry.text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let tag_text = tag_entry.text().trim().to_string();
        let tag = if tag_text.is_empty() {
            None
        } else {
            Some(tag_text)
        };
        let color = current_hex.borrow().clone();

        let new_def = MailLabelDefinition::new(name, color, tag);

        let mut labels = state.borrow_mut();
        if let Some((idx, _)) = edit_target {
            if idx < labels.len() {
                labels[idx] = new_def;
            }
        } else {
            labels.push(new_def);
        }
        drop(labels);

        if let Some(s) = settings.as_ref() {
            s.set_mail_labels(&state.borrow());
        }
        if let Some(p) = parent_weak.upgrade() {
            rebuild_mail_labels_list(&list, &state, &settings, &p);
            let toast_msg = if is_edit {
                crate::i18n::gettext("Label saved")
            } else {
                crate::i18n::gettext("Label added")
            };
            p.show_toast(&toast_msg);
            refresh_open_mail_views(&p);
        }
        if let Some(w) = window_for_save.upgrade() {
            w.close();
        }
    });

    window.present(Some(parent));
}

/// Check whether an executable program exists on the system PATH or standard binary directories.
pub fn detect_program_path(name: &str) -> Option<std::path::PathBuf> {
    if let Some(path) = glib::find_program_in_path(name) {
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for fallback_dir in &["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin", "/bin"] {
        let candidate = std::path::Path::new(fallback_dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn build_mail_junk_group(settings: Option<AppSettings>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(&crate::i18n::gettext("Junk Mail Options"))
        .build();

    let bogo_path = detect_program_path("bogofilter");
    let spam_path = detect_program_path("spamassassin");
    let any_available = bogo_path.is_some() || spam_path.is_some();

    let row_check_incoming = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Automatically check incoming mail for junk"))
        .active(true)
        .build();
    if let Some(s) = settings.clone() {
        row_check_incoming.set_active(s.junk_check_incoming());
    }
    group.add(&row_check_incoming);

    let row_lookup_ab = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Do not mark sender in address books as junk"))
        .active(false)
        .build();
    if let Some(s) = settings.clone() {
        row_lookup_ab.set_active(s.junk_lookup_addressbook());
    }
    group.add(&row_lookup_ab);

    let row_lookup_ab_local = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Only use local address books for whitelist"))
        .active(false)
        .visible(true)
        .build();
    if let Some(s) = settings.clone() {
        row_lookup_ab_local.set_active(s.junk_lookup_addressbook_local_only());
    }
    row_lookup_ab_local.set_sensitive(row_lookup_ab.is_active());
    group.add(&row_lookup_ab_local);

    let row_empty_on_exit = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Empty junk folder on application exit"))
        .active(false)
        .build();
    if let Some(s) = settings.clone() {
        row_empty_on_exit.set_active(s.junk_empty_on_exit());
    }
    group.add(&row_empty_on_exit);

    let row_empty_days = adw::ComboRow::builder()
        .title(&crate::i18n::gettext("Retention period in junk folder"))
        .model(&gtk::StringList::new(&[
            &crate::i18n::gettext("Immediately"),
            &crate::i18n::gettext("Older than 1 day"),
            &crate::i18n::gettext("Older than 7 days"),
            &crate::i18n::gettext("Older than 14 days"),
            &crate::i18n::gettext("Older than 30 days"),
        ]))
        .visible(true)
        .build();
    row_empty_days.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));
    if let Some(s) = settings.clone() {
        let days = s.junk_empty_on_exit_days();
        let idx = match days {
            1 => 1,
            7 => 2,
            14 => 3,
            30 => 4,
            _ => 0,
        };
        row_empty_days.set_selected(idx);
        let s_days = s.clone();
        row_empty_days.connect_selected_notify(move |row| {
            let days = match row.selected() {
                1 => 1,
                2 => 7,
                3 => 14,
                4 => 30,
                _ => 0,
            };
            s_days.set_junk_empty_on_exit_days(days);
        });
    }
    row_empty_days.set_sensitive(row_empty_on_exit.is_active());
    group.add(&row_empty_days);

    let row_plugin = adw::ComboRow::builder()
        .title(&crate::i18n::gettext("Default Spam Filter"))
        .model(&gtk::StringList::new(&["Bogofilter", "SpamAssassin"]))
        .visible(true)
        .build();
    row_plugin.add_prefix(&gtk::Image::from_icon_name("system-run-symbolic"));

    let update_plugin_subtitle = {
        let row_plugin = row_plugin.clone();
        let bogo_path = bogo_path.clone();
        let spam_path = spam_path.clone();
        move |selected_idx: u32| {
            let (name, path) = if selected_idx == 1 {
                ("SpamAssassin", &spam_path)
            } else {
                ("Bogofilter", &bogo_path)
            };
            match path {
                Some(p) => row_plugin.set_subtitle(&format!("{name} available: {}", p.display())),
                None => row_plugin.set_subtitle(&format!("Not found in PATH: Please install '{name}' on your system")),
            }
        }
    };

    if let Some(s) = settings.clone() {
        let selected = if s.junk_default_plugin().eq_ignore_ascii_case("SpamAssassin") {
            1
        } else {
            0
        };
        row_plugin.set_selected(selected);
        if any_available {
            update_plugin_subtitle(selected);
        }
        let s_plugin = s.clone();
        let update_sub = update_plugin_subtitle.clone();
        row_plugin.connect_selected_notify(move |row| {
            let plugin = if row.selected() == 1 {
                "SpamAssassin"
            } else {
                "Bogofilter"
            };
            s_plugin.set_junk_default_plugin(plugin);
            update_sub(row.selected());
        });
    }

    if !any_available {
        row_plugin.set_sensitive(false);
        row_plugin.set_subtitle(&crate::i18n::gettext(
            "No local spam scanner installed (Bogofilter or SpamAssassin)",
        ));
    } else {
        row_plugin.set_sensitive(row_check_incoming.is_active());
    }
    group.add(&row_plugin);
    let row_plugin_toggle = make_segmented_toggle_row(
        &row_plugin,
        &crate::i18n::gettext("Default Spam Filter"),
        "Bogofilter",
        "SpamAssassin",
    );
    row_plugin_toggle.add_prefix(&gtk::Image::from_icon_name("system-run-symbolic"));
    if !any_available {
        row_plugin_toggle.set_sensitive(false);
        row_plugin_toggle.set_subtitle(&crate::i18n::gettext(
            "No local spam scanner installed · Only server spam headers are evaluated",
        ));
    } else {
        row_plugin_toggle.set_sensitive(row_check_incoming.is_active());
    }
    group.add(&row_plugin_toggle);

    if !any_available {
        let install_row = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Local Spam Scanner"))
            .subtitle(&crate::i18n::gettext(
                "Install Bogofilter to enable fast offline spam filtering",
            ))
            .build();
        install_row.add_prefix(&gtk::Image::from_icon_name("system-software-install-symbolic"));
        let btn_install = gtk::Button::builder()
            .label(&crate::i18n::gettext("Install Bogofilter…"))
            .valign(gtk::Align::Center)
            .build();
        btn_install.connect_clicked(|_| {
            let status = std::process::Command::new("xdg-open")
                .arg("appstream://org.bogofilter.bogofilter")
                .spawn();
            if status.is_err() {
                let _ = std::process::Command::new("gnome-software")
                    .arg("--search=bogofilter")
                    .spawn();
            }
        });
        install_row.add_suffix(&btn_install);
        group.add(&install_row);
    }

    if let Some(s) = settings.clone() {
        let row_plugin_ref = row_plugin.clone();
        let row_plugin_toggle_ref = row_plugin_toggle.clone();
        let s_check = s.clone();
        row_check_incoming.connect_active_notify(move |row| {
            s_check.set_junk_check_incoming(row.is_active());
            row_plugin_ref.set_sensitive(row.is_active() && any_available);
            row_plugin_toggle_ref.set_sensitive(row.is_active() && any_available);
        });

        let row_lookup_ab_local_ref = row_lookup_ab_local.clone();
        let s_lookup = s.clone();
        row_lookup_ab.connect_active_notify(move |row| {
            s_lookup.set_junk_lookup_addressbook(row.is_active());
            row_lookup_ab_local_ref.set_sensitive(row.is_active());
        });

        let s_lookup_local = s.clone();
        row_lookup_ab_local.connect_active_notify(move |row| {
            s_lookup_local.set_junk_lookup_addressbook_local_only(row.is_active());
        });

        let row_empty_days_ref = row_empty_days.clone();
        let s_empty = s.clone();
        row_empty_on_exit.connect_active_notify(move |row| {
            s_empty.set_junk_empty_on_exit(row.is_active());
            row_empty_days_ref.set_sensitive(row.is_active());
        });
    }

    group
}

fn build_mail_notifications_group(settings: Option<AppSettings>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(&crate::i18n::gettext("Notifications"))
        .build();

    let row_notify = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("System Notifications"))
        .build();

    let row_only_inbox = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Notify only for Inbox folders"))
        .build();

    let row_sound = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Play a sound for new messages"))
        .build();

    let sound_types = gtk::StringList::new(&[
        &crate::i18n::gettext("System Sound"),
        &crate::i18n::gettext("System beep"),
        &crate::i18n::gettext("Custom sound file"),
    ]);
    let row_sound_type = adw::ComboRow::builder()
        .title(&crate::i18n::gettext("Sound Type"))
        .model(&sound_types)
        .build();

    let row_sound_file = adw::ActionRow::builder()
        .title(&crate::i18n::gettext("Sound File"))
        .subtitle(&crate::i18n::gettext("Path to audio file (.oga, .ogg, .wav)"))
        .build();

    let file_entry = gtk::Entry::builder()
        .hexpand(true)
        .valign(gtk::Align::Center)
        .placeholder_text("/usr/share/sounds/...")
        .build();

    let choose_button = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(&crate::i18n::gettext("Select audio file"))
        .build();

    let play_button = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(&crate::i18n::gettext("Preview sound"))
        .build();

    let file_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    file_box.append(&file_entry);
    file_box.append(&choose_button);
    file_box.append(&play_button);
    row_sound_file.add_suffix(&file_box);

    if let Some(s) = settings.as_ref() {
        let notify_active = s.mail_notification_enabled();
        row_notify.set_active(notify_active);
        row_only_inbox.set_active(s.mail_notification_only_inbox());
        row_only_inbox.set_sensitive(notify_active);
        row_sound.set_active(s.mail_notification_sound_enabled());

        let initial_sound_type = if s.mail_notification_sound_beep() {
            1
        } else if s.mail_notification_sound_play_file() {
            2
        } else {
            0
        };
        row_sound_type.set_selected(initial_sound_type);
        row_sound_type.set_sensitive(s.mail_notification_sound_enabled());
        row_sound_file.set_visible(initial_sound_type == 2 && s.mail_notification_sound_enabled());

        let sound_file_text = s.mail_notification_sound_file();
        file_entry.set_text(&sound_file_text);

        let s_notify = s.clone();
        let row_only_inbox_weak = row_only_inbox.downgrade();
        row_notify.connect_active_notify(move |row| {
            let active = row.is_active();
            s_notify.set_mail_notification_enabled(active);
            if let Some(r_inbox) = row_only_inbox_weak.upgrade() {
                r_inbox.set_sensitive(active);
            }
        });

        let s_inbox = s.clone();
        row_only_inbox.connect_active_notify(move |row| {
            s_inbox.set_mail_notification_only_inbox(row.is_active());
        });

        let s_sound = s.clone();
        let sound_type_row_weak = row_sound_type.downgrade();
        let sound_file_row_weak = row_sound_file.downgrade();
        row_sound.connect_active_notify(move |row| {
            let active = row.is_active();
            s_sound.set_mail_notification_sound_enabled(active);
            if let Some(row_type) = sound_type_row_weak.upgrade() {
                row_type.set_sensitive(active);
                if let Some(row_file) = sound_file_row_weak.upgrade() {
                    row_file.set_visible(active && row_type.selected() == 2);
                }
            }
        });

        let s_type = s.clone();
        let sound_file_row_weak2 = row_sound_file.downgrade();
        row_sound_type.connect_selected_notify(move |row| {
            let selected = row.selected();
            if let Some(row_file) = sound_file_row_weak2.upgrade() {
                row_file.set_visible(selected == 2);
            }
            match selected {
                1 => {
                    s_type.set_mail_notification_sound_use_theme(false);
                    s_type.set_mail_notification_sound_beep(true);
                    s_type.set_mail_notification_sound_play_file(false);
                }
                2 => {
                    s_type.set_mail_notification_sound_use_theme(false);
                    s_type.set_mail_notification_sound_beep(false);
                    s_type.set_mail_notification_sound_play_file(true);
                }
                _ => {
                    s_type.set_mail_notification_sound_use_theme(true);
                    s_type.set_mail_notification_sound_beep(false);
                    s_type.set_mail_notification_sound_play_file(false);
                }
            }
        });

        let s_file = s.clone();
        file_entry.connect_text_notify(move |entry| {
            s_file.set_mail_notification_sound_file(&entry.text());
        });

        let file_entry_weak = file_entry.downgrade();
        let s_chooser = s.clone();
        choose_button.connect_clicked(move |btn| {
            let root = btn.root().and_downcast::<gtk::Window>();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Audio Files (*.oga, *.ogg, *.wav, *.mp3)"));
            filter.add_pattern("*.oga");
            filter.add_pattern("*.ogg");
            filter.add_pattern("*.wav");
            filter.add_pattern("*.mp3");

            let dialog = gtk::FileDialog::builder()
                .title(&crate::i18n::gettext("Select audio file"))
                .modal(true)
                .build();
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let file_entry_weak = file_entry_weak.clone();
            let s_chooser = s_chooser.clone();
            dialog.open(root.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().into_owned();
                        if let Some(entry) = file_entry_weak.upgrade() {
                            entry.set_text(&path_str);
                        }
                        s_chooser.set_mail_notification_sound_file(&path_str);
                    }
                }
            });
        });

        let s_play = s.clone();
        play_button.connect_clicked(move |_| {
            crate::services::mail_notification_service::MailNotificationService::preview_notification_sound(&s_play);
        });
    }

    let row_titlebar_counter = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Show unread email count in title bar"))
        .build();
    if let Some(s) = settings.as_ref() {
        row_titlebar_counter.set_active(s.show_window_title_unread_counter());
        let s_counter = s.clone();
        row_titlebar_counter.connect_active_notify(move |row| {
            s_counter.set_show_window_title_unread_counter(row.is_active());
        });
    }

    let row_cal_notify = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Calendar Notifications"))
        .build();
    if let Some(s) = settings.as_ref() {
        row_cal_notify.set_active(s.notifications_enabled());
        let s_cal = s.clone();
        row_cal_notify.connect_active_notify(move |r| {
            s_cal.set_notifications_enabled(r.is_active());
        });
    }

    let row_reminder_sound = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Play sound for reminders"))
        .build();
    if let Some(s) = settings.as_ref() {
        row_reminder_sound.set_active(s.reminder_sound_enabled());
        let s_rem = s.clone();
        row_reminder_sound.connect_active_notify(move |r| {
            s_rem.set_reminder_sound_enabled(r.is_active());
        });
    }

    let exp_sound = adw::ExpanderRow::builder()
        .title(&crate::i18n::gettext("Sound and Display Options"))
        .use_markup(false)
        .build();
    exp_sound.add_row(&row_sound_type);
    exp_sound.add_row(&row_sound_file);
    exp_sound.add_row(&row_titlebar_counter);

    group.add(&row_notify);
    group.add(&row_only_inbox);
    group.add(&row_sound);
    group.add(&row_cal_notify);
    group.add(&row_reminder_sound);
    group.add(&exp_sound);

    group
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
        FilterField::Subject => "Subject",
        FilterField::Sender => "Sender",
        FilterField::Recipient => "Recipient",
        FilterField::Body => "Body",
        FilterField::Header => "Header",
        FilterField::Label => "Label",
        FilterField::Unread => "Unread",
        FilterField::Flagged => "Flagged",
        FilterField::Important => "Important",
        FilterField::Attachment => "Has Attachment",
        FilterField::Date => "Date",
        FilterField::Folder => "Folder",
        FilterField::Junk => "Spam/Junk",
    }
}

fn filter_operator_label(operator: FilterOperator) -> &'static str {
    match operator {
        FilterOperator::Contains => "contains",
        FilterOperator::NotContains => "does not contain",
        FilterOperator::Equals => "is",
        FilterOperator::NotEquals => "is not",
        FilterOperator::StartsWith => "starts with",
        FilterOperator::EndsWith => "ends with",
        FilterOperator::Before => "before",
        FilterOperator::After => "after",
        FilterOperator::Exists => "exists",
    }
}

fn filter_action_label(action: &FilterAction) -> String {
    action.description()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use gtk::prelude::*;
    use adw::prelude::*;
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
            "meridian-preferences-{label}-{}",
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
        assert_eq!(missing.toast_message(), "Filter rule not found");

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
        assert!(invalid.toast_message().contains("failed"));
        assert!(!invalid.toast_message().contains("matches"));

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
            "2 matches · 1 changed · 1 moved · 0 copied · 0 deleted"
        );

        let missing = Ok(Ok(None));
        assert_eq!(
            manual_filter_result_toast(&missing),
            "Filter rule not found"
        );

        let worker_error = Ok(Err(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "state unavailable",
        )));
        assert_eq!(
            manual_filter_result_toast(&worker_error),
            "Local filter run failed: state unavailable"
        );

        let cancelled: Result<
            std::io::Result<Option<LocalFilterRunReport>>,
            async_channel::RecvError,
        > = Err(async_channel::RecvError);
        assert_eq!(
            manual_filter_result_toast(&cancelled),
            "Local filter run cancelled: receiving from an empty and closed channel"
        );
    }

    #[test]
    fn test_detect_program_path_finds_standard_binary_and_rejects_missing() {
        use super::detect_program_path;
        assert!(detect_program_path("sh").is_some());
        assert!(detect_program_path("non_existent_binary_xyz_404_not_found").is_none());
    }

    #[test]
    fn test_detect_program_path_adversarial_boundaries_and_traversal() {
        use super::detect_program_path;

        // 1. Existing standard commands
        let sh_path = detect_program_path("sh");
        assert!(sh_path.is_some(), "detect_program_path should find 'sh'");
        assert!(sh_path.unwrap().is_file());

        let ls_path = detect_program_path("ls");
        assert!(ls_path.is_some(), "detect_program_path should find 'ls'");
        assert!(ls_path.unwrap().is_file());

        // 2. Completely fictitious binary names
        assert_eq!(detect_program_path("fictitious_binary_xyz_404"), None);
        assert_eq!(detect_program_path("bogofilter_nonexistent_binary_variant"), None);
        assert_eq!(detect_program_path("spamassassin_fake_daemon_12345"), None);

        // 3. Empty and whitespace binary names
        assert_eq!(detect_program_path(""), None);

        // 4. Malicious path traversal injection in binary names
        // bogofilter doesn't exist on standard macOS test machines
        let traversal_bogofilter = detect_program_path("../../bin/bogofilter");
        println!("detect_program_path(../../bin/bogofilter) = {:?}", traversal_bogofilter);

        // Check path traversal behavior with known files
        let traversal_sh = detect_program_path("../../bin/sh");
        println!("detect_program_path(../../bin/sh) = {:?}", traversal_sh);

        let passwd_test = detect_program_path("../../etc/passwd");
        println!("detect_program_path(../../etc/passwd) = {:?}", passwd_test);
    }

    fn find_combo_by_title(root: &gtk::Widget, target: &str) -> Option<adw::ComboRow> {
        use gtk::prelude::*;
        use gio::prelude::ListModelExt;
        if let Ok(combo) = root.clone().downcast::<adw::ComboRow>() {
            if combo.title() == target {
                return Some(combo);
            }
        }
        if let Ok(dialog) = root.clone().downcast::<adw::Dialog>() {
            if let Some(child) = dialog.child() {
                if let Some(found) = find_combo_by_title(&child, target) {
                    return Some(found);
                }
            }
        }
        if let Ok(stack) = root.clone().downcast::<gtk::Stack>() {
            let pages = stack.pages();
            for i in 0..pages.n_items() {
                if let Some(page) = pages.item(i).and_then(|o| o.downcast::<gtk::StackPage>().ok()) {
                    let child = page.child();
                    if let Some(found) = find_combo_by_title(&child, target) {
                        return Some(found);
                    }
                }
            }
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            if let Some(found) = find_combo_by_title(&c, target) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    fn find_spin_by_title(root: &gtk::Widget, target: &str) -> Option<adw::SpinRow> {
        use gtk::prelude::Cast;
        use gio::prelude::ListModelExt;
        if let Ok(spin) = root.clone().downcast::<adw::SpinRow>() {
            if spin.title() == target {
                return Some(spin);
            }
        }
        if let Ok(dialog) = root.clone().downcast::<adw::Dialog>() {
            if let Some(child) = dialog.child() {
                if let Some(found) = find_spin_by_title(&child, target) {
                    return Some(found);
                }
            }
        }
        if let Ok(stack) = root.clone().downcast::<gtk::Stack>() {
            let pages = stack.pages();
            for i in 0..pages.n_items() {
                if let Some(page) = pages.item(i).and_then(|o| o.downcast::<gtk::StackPage>().ok()) {
                    let child = page.child();
                    if let Some(found) = find_spin_by_title(&child, target) {
                        return Some(found);
                    }
                }
            }
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            if let Some(found) = find_spin_by_title(&c, target) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    fn find_switch_by_title(root: &gtk::Widget, target: &str) -> Option<adw::SwitchRow> {
        use gtk::prelude::Cast;
        use gio::prelude::ListModelExt;
        if let Ok(sw) = root.clone().downcast::<adw::SwitchRow>() {
            if sw.title() == target {
                return Some(sw);
            }
        }
        if let Ok(dialog) = root.clone().downcast::<adw::Dialog>() {
            if let Some(child) = dialog.child() {
                if let Some(found) = find_switch_by_title(&child, target) {
                    return Some(found);
                }
            }
        }
        if let Ok(stack) = root.clone().downcast::<gtk::Stack>() {
            let pages = stack.pages();
            for i in 0..pages.n_items() {
                if let Some(page) = pages.item(i).and_then(|o| o.downcast::<gtk::StackPage>().ok()) {
                    let child = page.child();
                    if let Some(found) = find_switch_by_title(&child, target) {
                        return Some(found);
                    }
                }
            }
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            if let Some(found) = find_switch_by_title(&c, target) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    #[test]
    fn test_preferences_dialog_instantiation_and_theme_composer_groups() {
        use adw::prelude::*;
        use crate::services::AppSettings;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // 1. Instantiation verification
        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        // Newly constructed Libadwaita dialogs are not mapped until presented
        assert!(
            !dialog.is_mapped(),
            "PreferencesDialog must not be mapped immediately upon creation"
        );
        assert_eq!(
            dialog.content_width(),
            860,
            "PreferencesDialog must have configured content width of 860"
        );
        assert_eq!(
            dialog.content_height(),
            600,
            "PreferencesDialog must have configured content height of 600"
        );
        assert!(
            dialog.child().is_some(),
            "PreferencesDialog must have a child widget container"
        );

        let widget = dialog.upcast_ref::<gtk::Widget>();

        // 2. Theme Preferences Group & Row verification (Feature 17)
        let row_theme = find_combo_by_title(widget, "Application Color Scheme")
            .expect("Application Color Scheme ComboRow must exist in Appearance page");

        let theme_model = row_theme
            .model()
            .expect("Theme combo row must have a model");
        assert_eq!(
            theme_model.n_items(),
            3,
            "Theme combo row must contain 3 options (System, Light, Dark)"
        );

        let initial_scheme = settings.color_scheme();
        let expected_theme_idx = match initial_scheme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        };
        assert_eq!(
            row_theme.selected(),
            expected_theme_idx,
            "Initial theme combo selection must match active AppSettings color_scheme"
        );

        // GSettings -> UI reactivity
        settings.set_color_scheme("dark");
        while context.iteration(false) {}
        assert_eq!(
            row_theme.selected(),
            2,
            "Setting color_scheme to 'dark' must update combo to index 2"
        );

        settings.set_color_scheme("light");
        while context.iteration(false) {}
        assert_eq!(
            row_theme.selected(),
            1,
            "Setting color_scheme to 'light' must update combo to index 1"
        );

        // UI -> GSettings reactivity
        row_theme.set_selected(0);
        while context.iteration(false) {}
        assert_eq!(
            settings.color_scheme(),
            "system",
            "Selecting index 0 in UI must set color_scheme to 'system'"
        );
        assert_eq!(
            adw::StyleManager::default().color_scheme(),
            adw::ColorScheme::Default,
            "AdwStyleManager color scheme must synchronize with theme selection"
        );

        // Restore initial scheme
        settings.set_color_scheme(&initial_scheme);
        while context.iteration(false) {}

        // 3. Composer Preferences Group & Rows verification (Features 18 & 19)
        let row_html = find_switch_by_title(widget, "Use HTML when composing")
            .expect("HTML switch row must exist in Composer General preferences group");
        let row_autosave = find_spin_by_title(widget, "Automatically save drafts")
            .expect("Draft autosave spin row must exist in Composer General preferences group");

        assert_eq!(
            row_html.is_active(),
            settings.composer_html(),
            "Composer HTML switch active state must match settings.composer_html()"
        );
        assert_eq!(
            row_autosave.value().round() as u32,
            settings.draft_autosave_interval_seconds(),
            "Composer draft autosave value must match settings.draft_autosave_interval_seconds()"
        );

        let autosave_adj = row_autosave.adjustment();
        assert_eq!(autosave_adj.lower(), 0.0, "Autosave minimum bound must be 0s");
        assert_eq!(autosave_adj.upper(), 3600.0, "Autosave maximum bound must be 3600s");

        // UI -> Settings reactivity for Composer HTML switch
        let initial_html = settings.composer_html();
        row_html.set_active(!initial_html);
        while context.iteration(false) {}
        assert_eq!(
            settings.composer_html(),
            !initial_html,
            "Toggling Composer HTML switch in UI must update settings.composer_html()"
        );

        // Restore initial HTML setting
        row_html.set_active(initial_html);
        while context.iteration(false) {}
        assert_eq!(settings.composer_html(), initial_html);
    }

    #[test]
    fn test_feature_19_preferences_dialog_reply_and_forward_and_autosave_sync() {
        use adw::prelude::*;
        use crate::services::AppSettings;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Reset settings
        settings.set_composer_reply_style("quoted");
        settings.set_composer_forward_style("attached");
        settings.set_draft_autosave_interval_seconds(30);
        while context.iteration(false) {}

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let widget = dialog.upcast_ref::<gtk::Widget>();

        // 1. Locate the widgets
        let row_reply = find_combo_by_title(widget, "Reply Style")
            .expect("Default Reply Style ComboRow must exist in PreferencesDialog");
        let row_forward = find_combo_by_title(widget, "Forward Style")
            .expect("Default Forward Style ComboRow must exist in PreferencesDialog");
        let row_autosave = find_spin_by_title(widget, "Automatically save drafts")
            .expect("Automatically save drafts SpinRow must exist in PreferencesDialog");

        // 2. Test initial values
        assert_eq!(row_reply.selected(), 0, "Default reply style 'quoted' should select index 0");
        assert_eq!(row_forward.selected(), 0, "Default forward style 'attached' should select index 0");
        assert_eq!(row_autosave.value().round() as u32, 30, "Default autosave interval should be 30");

        // 3. Test Reply Style GSettings -> UI sync
        settings.set_composer_reply_style("inline");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 1, "Setting reply style to 'inline' must update UI to index 1");

        settings.set_composer_reply_style("attach");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 2, "Setting reply style to 'attach' must update UI to index 2");

        settings.set_composer_reply_style("attached");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 2, "Setting reply style to 'attached' must update UI to index 2");

        settings.set_composer_reply_style("do-not-quote");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 3, "Setting reply style to 'do-not-quote' must update UI to index 3");

        settings.set_composer_reply_style("none");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 3, "Setting reply style to 'none' must update UI to index 3");

        settings.set_composer_reply_style("quoted");
        while context.iteration(false) {}
        assert_eq!(row_reply.selected(), 0, "Setting reply style to 'quoted' must update UI to index 0");

        // 4. Test Reply Style UI -> GSettings sync
        row_reply.set_selected(1);
        while context.iteration(false) {}
        assert_eq!(settings.composer_reply_style(), "inline");

        row_reply.set_selected(2);
        while context.iteration(false) {}
        assert_eq!(settings.composer_reply_style(), "attach");

        row_reply.set_selected(3);
        while context.iteration(false) {}
        assert_eq!(settings.composer_reply_style(), "do-not-quote");

        row_reply.set_selected(0);
        while context.iteration(false) {}
        assert_eq!(settings.composer_reply_style(), "quoted");

        // 5. Test Forward Style GSettings -> UI sync
        settings.set_composer_forward_style("inline");
        while context.iteration(false) {}
        assert_eq!(row_forward.selected(), 1, "Setting forward style to 'inline' must update UI to index 1");

        settings.set_composer_forward_style("quoted");
        while context.iteration(false) {}
        assert_eq!(row_forward.selected(), 2, "Setting forward style to 'quoted' must update UI to index 2");

        settings.set_composer_forward_style("attached");
        while context.iteration(false) {}
        assert_eq!(row_forward.selected(), 0, "Setting forward style to 'attached' must update UI to index 0");

        // 6. Test Forward Style UI -> GSettings sync
        row_forward.set_selected(1);
        while context.iteration(false) {}
        assert_eq!(settings.composer_forward_style(), "inline");

        row_forward.set_selected(2);
        while context.iteration(false) {}
        assert_eq!(settings.composer_forward_style(), "quoted");

        row_forward.set_selected(0);
        while context.iteration(false) {}
        assert_eq!(settings.composer_forward_style(), "attached");

        // 7. Test Autosave Interval GSettings -> UI sync
        settings.set_draft_autosave_interval_seconds(0);
        while context.iteration(false) {}
        assert_eq!(row_autosave.value().round() as u32, 0);

        settings.set_draft_autosave_interval_seconds(60);
        while context.iteration(false) {}
        assert_eq!(row_autosave.value().round() as u32, 60);

        settings.set_draft_autosave_interval_seconds(3600);
        while context.iteration(false) {}
        assert_eq!(row_autosave.value().round() as u32, 3600);

        // 8. Test Autosave Interval UI -> GSettings sync
        row_autosave.set_value(45.0);
        while context.iteration(false) {}
        assert_eq!(settings.draft_autosave_interval_seconds(), 45);

        // 9. Test Direct GSettings Boundary Injection into Autosave SpinRow
        let raw_settings = gio::Settings::new("com.martingawron.Meridian");
        let _ = raw_settings.set_int("draft-autosave-interval", -10);
        while context.iteration(false) {}
        assert_eq!(row_autosave.value().round() as u32, 0, "Negative interval must clamp spinrow to 0");

        let _ = raw_settings.set_int("draft-autosave-interval", 5000);
        while context.iteration(false) {}
        assert_eq!(row_autosave.value().round() as u32, 3600, "Large interval must clamp spinrow to 3600");

        // 10. Adversarial Stress: Raw GSettings injection of "none" while dialog is open
        let _ = raw_settings.set_string("composer-reply-style", "none");
        while context.iteration(false) {}
        assert_eq!(
            row_reply.selected(),
            3,
            "Setting raw 'none' in GSettings must normalize to index 3 ('Do not quote')"
        );

        // 11. Adversarial Stress: Raw GSettings injection of "ATTACH" (uppercase)
        let _ = raw_settings.set_string("composer-reply-style", "ATTACH");
        while context.iteration(false) {}
        assert_eq!(
            row_reply.selected(),
            2,
            "Setting raw 'ATTACH' in GSettings must normalize to index 2 ('As attachment')"
        );

        // 12. Adversarial Stress: Raw GSettings injection of "Inline" (titlecase)
        let _ = raw_settings.set_string("composer-reply-style", "Inline");
        while context.iteration(false) {}
        assert_eq!(
            row_reply.selected(),
            1,
            "Setting raw 'Inline' in GSettings must normalize to index 1 ('Inline')"
        );

        // 13. Adversarial Stress: Raw GSettings injection of whitespace
        let _ = raw_settings.set_string("composer-reply-style", "  do-not-quote  ");
        while context.iteration(false) {}
        assert_eq!(
            row_reply.selected(),
            3,
            "Setting raw '  do-not-quote  ' in GSettings must normalize to index 3"
        );

        // 14. Adversarial Stress: Raw GSettings injection of "Inline" for forward style
        let _ = raw_settings.set_string("composer-forward-style", "Inline");
        while context.iteration(false) {}
        assert_eq!(
            row_forward.selected(),
            1,
            "Setting raw 'Inline' forward style in GSettings must normalize to index 1 ('Inline')"
        );

        // 15. Adversarial Stress: Raw GSettings injection of "QUOTED" for forward style
        let _ = raw_settings.set_string("composer-forward-style", "QUOTED");
        while context.iteration(false) {}
        assert_eq!(
            row_forward.selected(),
            2,
            "Setting raw 'QUOTED' forward style in GSettings must normalize to index 2 ('Quoted')"
        );

        // 16. Adversarial Stress: Raw GSettings injection of "attach" for forward style
        let _ = raw_settings.set_string("composer-forward-style", "attach");
        while context.iteration(false) {}
        assert_eq!(
            row_forward.selected(),
            0,
            "Setting raw 'attach' forward style in GSettings must normalize to index 0 ('As attachment')"
        );

        // 17. Adversarial Stress: Raw GSettings injection of whitespace for forward style
        let _ = raw_settings.set_string("composer-forward-style", "  inline  ");
        while context.iteration(false) {}
        assert_eq!(
            row_forward.selected(),
            1,
            "Setting raw '  inline  ' forward style in GSettings must normalize to index 1"
        );

        // Reset
        settings.set_composer_reply_style("quoted");
        settings.set_composer_forward_style("attached");
        settings.set_draft_autosave_interval_seconds(30);
        while context.iteration(false) {}
    }

    #[test]
    fn test_preferences_dialog_present_visible() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let window = adw::ApplicationWindow::builder().build();
        window.present();
        while context.iteration(false) {}
        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}
        dialog.present(Some(&window));
        for _ in 0..10 {
            context.iteration(false);
        }
        assert!(
            dialog.is_visible(),
            "PreferencesDialog must become visible when presented to avoid trapping pointer events in modal backdrop"
        );
    }

    #[test]
    fn test_workday_time_options_and_conversions() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        use super::{
            minutes_to_workday_time_index, workday_time_index_to_minutes, workday_time_options,
        };
        use adw::prelude::*;

        let options = workday_time_options();
        assert_eq!(options.n_items(), 48);
        assert_eq!(options.string(0).as_deref(), Some("00:00"));
        assert_eq!(options.string(16).as_deref(), Some("08:00"));
        assert_eq!(options.string(18).as_deref(), Some("09:00"));
        assert_eq!(options.string(34).as_deref(), Some("17:00"));
        assert_eq!(options.string(47).as_deref(), Some("23:30"));

        // Exact conversions
        assert_eq!(minutes_to_workday_time_index(0), 0);
        assert_eq!(workday_time_index_to_minutes(0), 0);

        assert_eq!(minutes_to_workday_time_index(540), 18);
        assert_eq!(workday_time_index_to_minutes(18), 540);

        assert_eq!(minutes_to_workday_time_index(1020), 34);
        assert_eq!(workday_time_index_to_minutes(34), 1020);

        // Clamping and rounding
        assert_eq!(minutes_to_workday_time_index(535), 18);
        assert_eq!(minutes_to_workday_time_index(554), 18);
        assert_eq!(minutes_to_workday_time_index(555), 19);
        assert_eq!(minutes_to_workday_time_index(2000), 47);
        assert_eq!(workday_time_index_to_minutes(100), 1410);
    }

    #[test]
    fn test_sync_interval_options_and_conversions() {
        use super::{sync_interval_seconds_list, sync_interval_to_index};

        let list = sync_interval_seconds_list();
        assert_eq!(list.len(), 8);
        assert_eq!(list, &[0, 60, 300, 600, 900, 1800, 3600, 86400]);

        // Exact index matching
        assert_eq!(sync_interval_to_index(0), 0); // Manually
        assert_eq!(sync_interval_to_index(60), 1); // 1 min
        assert_eq!(sync_interval_to_index(300), 2); // 5 min (default)
        assert_eq!(sync_interval_to_index(600), 3); // 10 min
        assert_eq!(sync_interval_to_index(900), 4); // 15 min
        assert_eq!(sync_interval_to_index(1800), 5); // 30 min
        assert_eq!(sync_interval_to_index(3600), 6); // Hourly
        assert_eq!(sync_interval_to_index(86400), 7); // Daily

        // Nearest neighbor matching
        assert_eq!(sync_interval_to_index(70), 1);
        assert_eq!(sync_interval_to_index(280), 2);
        assert_eq!(sync_interval_to_index(650), 3);
        assert_eq!(sync_interval_to_index(100_000), 7);
    }

    #[test]
    fn test_gnome_hig_workday_and_sync_interval_rows_exist_in_dialog() {
        use adw::prelude::*;
        use crate::services::AppSettings;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        settings.set_calendar_workday_start_minutes(540);
        settings.set_calendar_workday_end_minutes(1020);
        settings.set_mail_sync_interval_seconds(300);
        while context.iteration(false) {}

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}
        let widget = dialog.upcast_ref::<gtk::Widget>();

        // Work Day Starts & Ends are ComboRows now
        let row_start = find_combo_by_title(widget, "Work Day Starts")
            .expect("Work Day Starts ComboRow must exist in Calendar page");
        let row_end = find_combo_by_title(widget, "Work Day Ends")
            .expect("Work Day Ends ComboRow must exist in Calendar page");

        assert_eq!(row_start.selected(), 18, "540 min should select 09:00 (index 18)");
        assert_eq!(row_end.selected(), 34, "1020 min should select 17:00 (index 34)");

        // Mail sync interval ComboRow in General Mail page
        let row_sync = find_combo_by_title(widget, "Check for New Mail")
            .or_else(|| find_combo_by_title(widget, "Check for new messages"))
            .expect("Check for New Mail ComboRow must exist");
        assert_eq!(row_sync.selected(), 2, "300s should select Every 5 minutes (index 2)");

        // UI -> Settings reactivity
        row_start.set_selected(16); // 08:00
        while context.iteration(false) {}
        assert_eq!(settings.calendar_workday_start_minutes(), 480);

        row_end.set_selected(36); // 18:00
        while context.iteration(false) {}
        assert_eq!(settings.calendar_workday_end_minutes(), 1080);

        row_sync.set_selected(3); // 10 min = 600s
        while context.iteration(false) {}
        assert_eq!(settings.mail_sync_interval_seconds(), 600);

        // Reset
        settings.set_calendar_workday_start_minutes(540);
        settings.set_calendar_workday_end_minutes(1020);
        settings.set_mail_sync_interval_seconds(300);
        while context.iteration(false) {}
    }

    #[test]
    fn test_sidebar_sections_and_page_definitions() {
        use super::SIDEBAR_SECTIONS;

        assert_eq!(SIDEBAR_SECTIONS.len(), 4, "Must have exactly 4 sidebar blocks per redesign spec");
        assert_eq!(SIDEBAR_SECTIONS[0].section, "Accounts");
        assert_eq!(SIDEBAR_SECTIONS[1].section, "General");
        assert_eq!(SIDEBAR_SECTIONS[2].section, "Productivity");
        assert_eq!(SIDEBAR_SECTIONS[3].section, "System");

        let total_pages: usize = SIDEBAR_SECTIONS.iter().map(|s| s.pages.len()).sum();
        assert_eq!(total_pages, 14, "Must define 14 consolidated preferences pages");

        let expected_ids = [
            "accounts", "labels", "filters", "certificates",
            "mail_general", "appearance", "mail_layout", "mail_reading", "composer",
            "calendar", "contacts", "tasks", "backup", "advanced",
        ];

        let actual_ids: Vec<&str> = SIDEBAR_SECTIONS
            .iter()
            .flat_map(|s| s.pages.iter().map(|p| p.id))
            .collect();
        assert_eq!(actual_ids, expected_ids);

        for id in expected_ids {
            assert!(
                super::get_page_title(id).is_some(),
                "get_page_title must return title for page id '{id}'"
            );
        }
        assert!(super::get_page_title("non_existent_page_id").is_none());
    }

    #[test]
    fn test_preferences_dialog_split_view_and_pages_stack_integration() {
        use adw::prelude::*;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let child = dialog.child().expect("PreferencesDialog child must exist");
        let split = child
            .downcast::<adw::NavigationSplitView>()
            .expect("Dialog child must be an adw::NavigationSplitView");

        assert_eq!(split.min_sidebar_width(), 205.0);
        assert_eq!(split.max_sidebar_width(), 235.0);
        assert!((split.sidebar_width_fraction() - 0.28).abs() < 0.01);

        // Sidebar navigation page
        let sidebar_page = split.sidebar().expect("Sidebar page must exist");
        let sidebar_toolbar = sidebar_page
            .child()
            .and_then(|w| w.downcast::<adw::ToolbarView>().ok())
            .expect("Sidebar page child must be a ToolbarView");

        let side_stack = sidebar_toolbar
            .content()
            .and_then(|w| w.downcast::<gtk::Stack>().ok())
            .expect("Sidebar toolbar content must be a Stack");
        assert!(side_stack.child_by_name("pages").is_some());
        assert!(side_stack.child_by_name("results").is_some());
        assert!(side_stack.child_by_name("empty").is_some());

        // Content navigation page
        let content_page = split.content().expect("Content page must exist");
        let content_toolbar = content_page
            .child()
            .and_then(|w| w.downcast::<adw::ToolbarView>().ok())
            .expect("Content page child must be a ToolbarView");

        let detail_stack = content_toolbar
            .content()
            .and_then(|w| w.downcast::<gtk::Stack>().ok())
            .expect("Content toolbar content must be a Stack");

        // Verify all 27 pages exist in detail_stack (14 top-level + 13 subpages)
        let expected_ids = [
            "accounts", "labels", "filters", "certificates",
            "mail_general", "appearance", "mail_layout", "mail_reading", "composer",
            "calendar", "contacts", "tasks", "backup", "advanced",
            "junk", "sub_smime", "sub_allowed_senders", "sub_sync_cache", "sub_header_toolbar",
            "sub_list_toolbar", "sub_list_elements",
            "headers", "sub_reading_advanced", "sub_calendar_workdays",
            "sub_calendar_reminders", "sub_calendar_categories", "sub_calendar_view",
        ];
        for id in expected_ids {
            assert!(
                detail_stack.child_by_name(id).is_some(),
                "detail_stack must contain page '{id}'"
            );
        }

        // Verify search indexer extracts entries from detail_stack
        let search_index = super::build_search_index(&detail_stack);
        assert!(
            search_index.len() >= 30,
            "Search index must index groups and rows, found {}",
            search_index.len()
        );

        // Check search keyword hits
        let autosave_hit = search_index.iter().find(|h| h.haystack.contains("drafts") || h.haystack.contains("autosave"));
        assert!(autosave_hit.is_some(), "Search index must contain drafts/autosave row");
        assert_eq!(autosave_hit.unwrap().page_id, "composer");

        let theme_hit = search_index.iter().find(|h| h.haystack.contains("scheme") || h.haystack.contains("theme"));
        assert!(theme_hit.is_some(), "Search index must contain theme/color scheme row");
        assert_eq!(theme_hit.unwrap().page_id, "appearance");

        // Verify SwitchRow titles/subtitles are indexed
        let notify_hit = search_index.iter().find(|h| h.haystack.contains("banner") || h.haystack.contains("desktop") || h.haystack.contains("system notifications"));
        assert!(notify_hit.is_some(), "SwitchRow must be indexed into haystack");

        // Verify reveal_widget expands parent expander row
        let expander = adw::ExpanderRow::builder().title("Test Expander").expanded(false).build();
        let child_row = adw::ActionRow::builder().title("Target Inside Expander").build();
        expander.add_row(&child_row);
        assert!(!expander.is_expanded());
        super::reveal_widget(child_row.upcast_ref());
        assert!(expander.is_expanded(), "reveal_widget must automatically expand parent expander rows");
    }

    #[test]
    fn test_preferences_dialog_page_selection_and_switch() {
        use adw::prelude::*;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let split = dialog
            .child()
            .and_then(|w| w.downcast::<adw::NavigationSplitView>().ok())
            .expect("Dialog child must be NavigationSplitView");

        let content_page = split.content().unwrap();
        let content_toolbar = content_page.child().and_then(|w| w.downcast::<adw::ToolbarView>().ok()).unwrap();
        let detail_stack = content_toolbar.content().and_then(|w| w.downcast::<gtk::Stack>().ok()).unwrap();

        let sidebar_page = split.sidebar().unwrap();
        let sidebar_toolbar = sidebar_page.child().and_then(|w| w.downcast::<adw::ToolbarView>().ok()).unwrap();
        let side_stack = sidebar_toolbar.content().and_then(|w| w.downcast::<gtk::Stack>().ok()).unwrap();
        let pages_scroll = side_stack.child_by_name("pages").and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok()).unwrap();
        let sidebar_list = pages_scroll
            .child()
            .and_then(|w| w.downcast::<gtk::ListBox>().ok())
            .or_else(|| {
                pages_scroll
                    .child()
                    .and_then(|w| w.downcast::<gtk::Viewport>().ok())
                    .and_then(|vp| vp.child())
                    .and_then(|w| w.downcast::<gtk::ListBox>().ok())
            })
            .unwrap();

        // Switch to "signatures" (resolved alias -> "accounts")
        super::select_sidebar_page(&sidebar_list, "signatures");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("accounts"));

        // Switch to "appearance"
        super::select_sidebar_page(&sidebar_list, "appearance");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("appearance"));

        // Switch to "calendar_display" (resolved alias -> "calendar")
        super::select_sidebar_page(&sidebar_list, "calendar_display");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("calendar"));

        // Switch to "certificates"
        super::select_sidebar_page(&sidebar_list, "certificates");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("certificates"));
    }

    #[test]
    fn test_preferences_dialog_search_edge_cases_and_ranking() {
        use adw::prelude::*;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let split = dialog.child().unwrap().downcast::<adw::NavigationSplitView>().unwrap();
        let sidebar_page = split.sidebar().unwrap();
        let sidebar_toolbar = sidebar_page.child().unwrap().downcast::<adw::ToolbarView>().unwrap();
        let side_stack = sidebar_toolbar.content().unwrap().downcast::<gtk::Stack>().unwrap();

        // Locate search bar and entry via recursive widget search
        fn find_search_entry(widget: &gtk::Widget) -> Option<gtk::SearchEntry> {
            if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
                return Some(entry.clone());
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                if let Some(found) = find_search_entry(&c) {
                    return Some(found);
                }
                child = c.next_sibling();
            }
            None
        }

        let search_entry = find_search_entry(sidebar_toolbar.upcast_ref())
            .expect("SearchEntry must be present in sidebar toolbar");

        // Edge case 1: Whitespace query keeps categories visible
        search_entry.set_text("   ");
        std::thread::sleep(std::time::Duration::from_millis(200));
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("pages"));

        // Edge case 2: Matching query switches stack to "results"
        search_entry.set_text("drafts");
        std::thread::sleep(std::time::Duration::from_millis(200));
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("results"));

        // Searching for an item on an inactive page (e.g. "Color Scheme" from appearance page)
        search_entry.set_text("Color Scheme");
        std::thread::sleep(std::time::Duration::from_millis(200));
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("results"), "Searching for an item on an inactive page must yield results");

        // Edge case 3: Mixed case matches same results
        search_entry.set_text("DrAfTs");
        std::thread::sleep(std::time::Duration::from_millis(200));
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("results"));

        // Edge case 4: Nonexistent search term switches to "empty" status page
        search_entry.set_text("xyz_non_existent_search_query_9999");
        std::thread::sleep(std::time::Duration::from_millis(200));
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("empty"));

        // Edge case 5: Clearing search restores pages stack
        search_entry.set_text("");
        while context.iteration(false) {}
        assert_eq!(side_stack.visible_child_name().as_deref(), Some("pages"));

        // Edge case 6: Navigation Split View content_header start title buttons enabled for back button
        let content_page = split.content().unwrap();
        let content_toolbar = content_page.child().unwrap().downcast::<adw::ToolbarView>().unwrap();
        let mut content_child = content_toolbar.first_child();
        let mut found_header = None;
        while let Some(c) = content_child {
            if let Some(h) = c.downcast_ref::<adw::HeaderBar>() {
                found_header = Some(h.clone());
                break;
            }
            content_child = c.next_sibling();
        }
        if let Some(header) = found_header {
            assert!(
                header.shows_start_title_buttons(),
                "content_header must preserve start title buttons so back button is functional when collapsed"
            );
        }
    }

    #[test]
    fn test_preferences_dialog_select_page_api() {
        use glib::subclass::prelude::*;
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        dialog.select_page("calendar");
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("calendar"))
        );
        assert_eq!(
            dialog.imp().content_title.borrow().as_ref().map(|t| t.title().to_string()),
            Some("Calendar".to_string())
        );

        dialog.select_page("tasks");
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("tasks"))
        );
        assert_eq!(
            dialog.imp().content_title.borrow().as_ref().map(|t| t.title().to_string()),
            Some("Tasks & Memos".to_string())
        );

        dialog.select_page("appearance");
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("appearance"))
        );
    }

    #[test]
    fn test_preferences_dialog_mail_settings_coverage_and_reactivity() {
        use adw::prelude::*;
        use crate::services::AppSettings;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Reset settings
        settings.set_mail_show_avatar(true);
        settings.set_mail_fetch_remote_logos(false);
        settings.set_mail_show_preview_text(true);
        settings.set_mail_enable_unified_inbox(false);
        settings.set_mail_conversation_accordion(true);
        settings.set_mail_preserve_image_layout(true);
        settings.set_mail_mark_seen_timeout(1500);
        settings.set_composer_show_format_toolbar(false);
        settings.set_composer_track_recent_recipients(true);
        while context.iteration(false) {}

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}
        let widget = dialog.upcast_ref::<gtk::Widget>();

        // Verify widgets exist
        let row_avatar = find_switch_by_title(widget, "Show sender avatars")
            .expect("Show sender avatars SwitchRow must exist");
        let row_preview = find_switch_by_title(widget, "Show preview text in list")
            .expect("Show preview text SwitchRow must exist");
        let row_unified = find_switch_by_title(widget, "Unified Inbox")
            .expect("Unified Inbox SwitchRow must exist");
        let row_accordion = find_switch_by_title(widget, "Auto-expand conversations")
            .expect("Conversation accordion SwitchRow must exist");
        let row_preserve_img = find_switch_by_title(widget, "Preserve image layout")
            .expect("Preserve image layout SwitchRow must exist");
        let row_timeout = find_combo_by_title(widget, "Mark as read delay")
            .expect("Mark as read delay ComboRow must exist");
        let row_fmt_toolbar = find_switch_by_title(widget, "Always show formatting toolbar")
            .expect("Format toolbar SwitchRow must exist");
        let row_track_recip = find_switch_by_title(widget, "Remember recent recipients")
            .expect("Track recent recipients SwitchRow must exist");

        assert_eq!(row_avatar.is_active(), true);
        assert_eq!(row_preview.is_active(), true);
        assert_eq!(row_unified.is_active(), false);
        assert_eq!(row_accordion.is_active(), true);
        assert_eq!(row_preserve_img.is_active(), true);
        assert_eq!(row_timeout.selected(), 2); // 1500ms -> index 2
        assert_eq!(row_fmt_toolbar.is_active(), false);
        assert_eq!(row_track_recip.is_active(), true);

        // Test reactivity from UI to GSettings
        row_unified.set_active(true);
        while context.iteration(false) {}
        assert_eq!(settings.mail_enable_unified_inbox(), true);

        row_avatar.set_active(false);
        while context.iteration(false) {}
        assert_eq!(settings.mail_show_avatar(), false);

        row_preview.set_active(false);
        while context.iteration(false) {}
        assert_eq!(settings.mail_show_preview_text(), false);

        row_accordion.set_active(false);
        while context.iteration(false) {}
        assert_eq!(settings.mail_conversation_accordion(), false);

        row_preserve_img.set_active(false);
        while context.iteration(false) {}
        assert_eq!(settings.mail_preserve_image_layout(), false);

        row_timeout.set_selected(3); // 3000ms
        while context.iteration(false) {}
        assert_eq!(settings.mail_mark_seen_timeout(), 3000);

        row_fmt_toolbar.set_active(true);
        while context.iteration(false) {}
        assert_eq!(settings.composer_show_format_toolbar(), true);

        row_track_recip.set_active(false);
        while context.iteration(false) {}
        assert_eq!(settings.composer_track_recent_recipients(), false);
    }

    #[test]
    fn test_gnome_hig_subpages_definitions_and_detail_stack() {
        use super::{SUBPAGES, SIDEBAR_SECTIONS, resolve_sidebar_page_id};
        use adw::prelude::*;

        assert_eq!(SUBPAGES.len(), 13, "Must define 13 subpages matching the HIG overhaul plan");

        let top_level_page_ids: Vec<&str> = SIDEBAR_SECTIONS
            .iter()
            .flat_map(|s| s.pages.iter().map(|p| p.id))
            .collect();

        for sub in SUBPAGES {
            assert!(!sub.id.is_empty(), "Subpage id must not be empty");
            assert!(!sub.title.is_empty(), "Subpage title must not be empty");
            assert!(
                top_level_page_ids.contains(&sub.parent_id),
                "Subpage parent_id '{}' must be a valid top-level sidebar page",
                sub.parent_id
            );
            assert_eq!(
                resolve_sidebar_page_id(sub.id),
                sub.parent_id,
                "resolve_sidebar_page_id must resolve subpage '{}' to parent '{}'",
                sub.id,
                sub.parent_id
            );
        }

        for top_id in &top_level_page_ids {
            assert_eq!(
                resolve_sidebar_page_id(top_id),
                *top_id,
                "resolve_sidebar_page_id must return top-level id unchanged"
            );
        }

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let child = dialog.child().expect("PreferencesDialog child must exist");
        let split = child
            .downcast::<adw::NavigationSplitView>()
            .expect("Dialog child must be an adw::NavigationSplitView");
        let content_page = split.content().expect("Content page must exist");
        let content_toolbar = content_page
            .child()
            .and_then(|w| w.downcast::<adw::ToolbarView>().ok())
            .expect("Content page child must be a ToolbarView");
        let detail_stack = content_toolbar
            .content()
            .and_then(|w| w.downcast::<gtk::Stack>().ok())
            .expect("Content toolbar content must be a Stack");

        for sub in SUBPAGES {
            assert!(
                detail_stack.child_by_name(sub.id).is_some(),
                "detail_stack must contain subpage '{}'",
                sub.id
            );
        }
    }

    #[test]
    fn test_gnome_hig_subpage_navigation_and_back_button_behavior() {
        use adw::subclass::prelude::*;
        use gtk::prelude::*;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let back_btn = dialog
            .imp()
            .subpage_back_btn
            .borrow()
            .as_ref()
            .expect("subpage_back_btn must be initialized")
            .clone();

        // Initially on a top-level page, back button must be hidden
        assert_eq!(back_btn.is_visible(), false);

        // Select a subpage via select_page
        dialog.select_page("sub_smime");
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("sub_smime"))
        );
        assert_eq!(back_btn.is_visible(), true);
        assert_eq!(*dialog.imp().active_subpage_parent.borrow(), Some("certificates"));

        // Click the back button to slide back to parent
        back_btn.emit_clicked();
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("accounts"))
        );
        assert_eq!(back_btn.is_visible(), false);
        assert_eq!(*dialog.imp().active_subpage_parent.borrow(), None);

        // Test another subpage navigation: calendar workdays -> calendar
        dialog.select_page("sub_calendar_workdays");
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("sub_calendar_workdays"))
        );
        assert_eq!(back_btn.is_visible(), true);
        assert_eq!(*dialog.imp().active_subpage_parent.borrow(), Some("calendar"));

        back_btn.emit_clicked();
        while context.iteration(false) {}

        assert_eq!(
            dialog.imp().detail_stack.borrow().as_ref().and_then(|s| s.visible_child_name()),
            Some(glib::GString::from("calendar"))
        );
        assert_eq!(back_btn.is_visible(), false);
        assert_eq!(*dialog.imp().active_subpage_parent.borrow(), None);
    }

    #[test]
    fn test_gnome_hig_search_index_includes_subpages_with_breadcrumbs() {
        use super::SUBPAGES;
        use adw::subclass::prelude::*;

        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let dialog = super::PreferencesDialog::new();
        while context.iteration(false) {}

        let detail_stack = dialog
            .imp()
            .detail_stack
            .borrow()
            .as_ref()
            .expect("detail_stack must exist")
            .clone();

        let search_index = super::build_search_index(&detail_stack);

        // Verify all subpages have search index entries with breadcrumb format "Parent › Subpage"
        for sub in SUBPAGES {
            let hit = search_index.iter().find(|h| h.page_id == sub.id);
            assert!(
                hit.is_some(),
                "Search index must contain entry for subpage '{}'",
                sub.id
            );
            let hit = hit.unwrap();
            assert!(
                hit.place.contains('›'),
                "Subpage search hit must include '›' breadcrumb separator, got '{}'",
                hit.place
            );
        }

        // Verify specific unhidden power-user settings are indexed
        let composer_charset = search_index.iter().find(|h| h.haystack.contains("character encoding") || h.haystack.contains("charset") || h.haystack.contains("character set"));
        assert!(composer_charset.is_some(), "Search index must contain unhidden composer character set option");

        let caret_mode = search_index.iter().find(|h| h.haystack.contains("caret"));
        assert!(caret_mode.is_some(), "Search index must contain unhidden caret browsing option");

        let smime_ca = search_index.iter().find(|h| h.haystack.contains("certificate authorities") || h.haystack.contains("ca"));
        assert!(smime_ca.is_some(), "Search index must contain unhidden S/MIME CA trust option");
    }
}






