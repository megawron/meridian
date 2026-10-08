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
use crate::dialogs::SignatureEditorDialog;
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
            self.obj().set_content_width(980);
            self.obj().set_content_height(720);
            self.obj().build_ui();
            self.obj().connect_closed(|dialog| {
                if let Some(window) = crate::window::find_meridian_window(dialog) {
                    window.reload_accounts();
                }
            });
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
        let page = self.downgrade();
        dialog.connect_closed(move |_| {
            if let Some(page) = page.upgrade() {
                page.refresh_managed_account_rows();
                page.refresh_local_profile_rows();
                if let Some(window) = crate::window::find_meridian_window(&page) {
                    window.reload_accounts();
                }
            }
        });
        dialog.present(Some(self));
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
                .title("No local account profiles")
                .subtitle("Create a profile via \"Add New Account…\"")
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
            if profile.is_shared_mailbox {
                extras.push("Shared Mailbox");
            }
            if is_default {
                extras.push("Default Account");
            }
            if let Some(ref sig) = profile.signature_id {
                if !sig.is_empty() {
                    extras.push("Signature configured");
                }
            }
            if !profile.pgp_key_id.is_empty() {
                extras.push("PGP");
            }
            if profile.auto_check_enabled {
                extras.push("Auto-check");
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
            let icon_name = if profile.is_shared_mailbox {
                "system-users-symbolic"
            } else {
                "avatar-default-symbolic"
            };
            row.add_prefix(&gtk::Image::from_icon_name(icon_name));
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
                .tooltip_text("Edit custom account settings")
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
                            page.refresh_local_profile_rows();
                            if let Some(window) = crate::window::find_meridian_window(&page) {
                                window.reorder_account_roots();
                            }
                        }
                        Ok(false) => page.show_toast("Account profile not found"),
                        Err(error) => page.show_toast(&format!(
                            "Could not save account order: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&move_up);

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
                            page.refresh_local_profile_rows();
                            if let Some(window) = crate::window::find_meridian_window(&page) {
                                window.reorder_account_roots();
                            }
                        }
                        Ok(false) => page.show_toast("Account profile not found"),
                        Err(error) => page.show_toast(&format!(
                            "Could not save account order: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&move_down);

            let enabled = gtk::Switch::builder()
                .active(profile.enabled)
                .valign(gtk::Align::Center)
                .tooltip_text("Enable/disable profile")
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
                            page.refresh_managed_account_rows();
                            page.refresh_local_profile_rows();
                            if let Some(window) = crate::window::find_meridian_window(&page) {
                                window.reload_accounts();
                            }
                        }
                        Ok(false) => page.show_toast("Account profile not found"),
                        Err(error) => page.show_toast(&format!(
                            "Could not update account profile: {error}"
                        )),
                    }
                }
            });
            row.add_suffix(&enabled);

            let default_tooltip = if is_default {
                crate::i18n::gettext("Current default account")
            } else {
                crate::i18n::gettext("Use as default account for new messages")
            };
            let default_button = gtk::Button::builder()
                .icon_name(if is_default {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                })
                .tooltip_text(&default_tooltip)
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
                    page.show_toast(&crate::i18n::gettext("Default account saved"));
                    page.refresh_local_profile_rows();
                }
            });
            row.add_suffix(&default_button);

            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove local profile")
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
                    "Remove local account profile?",
                    "Only the application-owned, secret-free profile will be removed. Accounts and credentials remain unchanged.",
                    "Remove",
                    move || match AccountConfigStore::default().delete(&profile_id) {
                        Ok(true) => {
                            page_for_action.reconcile_local_default(&profile_id);
                            page_for_action.refresh_managed_account_rows();
                            page_for_action.refresh_local_profile_rows();
                            page_for_action.show_toast("Local account profile removed");
                            if let Some(window) = crate::window::find_meridian_window(&page_for_action) {
                                window.reload_accounts();
                            }
                        }
                        Ok(false) => {
                            page_for_action.show_toast("Local account profile not found")
                        }
                        Err(error) => page_for_action.show_toast(&format!(
                            "Could not remove account profile: {error}"
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
            // If already shown in non-shared local profiles, do not duplicate
            let is_local = profiles.iter().any(|p| {
                !p.is_shared_mailbox
                    && (source.uid == p.id
                        || source.uid == format!("eds:{}", p.id)
                        || p.id == format!("eds:{}", source.uid)
                        || (!p.email.is_empty() && source.display_name.eq_ignore_ascii_case(&p.email))
                        || (!p.email.is_empty() && source.display_name.contains(&p.email))
                        || source.display_name.eq_ignore_ascii_case(&p.display_name))
            });
            if !is_shared && !is_local {
                visible_eds_sources.push(source);
            }
        }

        if visible_eds_sources.is_empty() && shared_profiles.is_empty() {
            if profiles.is_empty() {
                let no_accounts = adw::ActionRow::builder()
                    .title("No active accounts found")
                    .subtitle("Use \"Add New Account…\" or \"Open Shared Mailbox…\"")
                    .build();
                let icon_no_accounts = gtk::Image::from_icon_name("dialog-information-symbolic");
                no_accounts.add_prefix(&icon_no_accounts);
                group.add(&no_accounts);
                self.imp()
                    .managed_account_rows
                    .borrow_mut()
                    .push(no_accounts.upcast());
            }
            return;
        }

        // 1. Regular EDS mail sources
        for source in visible_eds_sources {
            let backend = if source.backend_name.trim().is_empty() {
                "IMAP".to_string()
            } else {
                source.backend_name.trim().to_uppercase()
            };
            let state = if source.enabled {
                crate::i18n::gettext("Active")
            } else {
                crate::i18n::gettext("Disabled")
            };
            let subtitle = if source.display_name.contains('@') {
                format!("{backend} · {state}")
            } else {
                format!("Online Account · {backend} · {state}")
            };
            let row = adw::ActionRow::builder()
                .title(&source.display_name)
                .subtitle(subtitle)
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
                .tooltip_text("Customize settings for this EDS account")
                .css_classes(vec!["flat"])
                .valign(gtk::Align::Center)
                .build();
            edit_btn.connect_clicked(move |_| {
                open_eds_editor();
            });
            row.add_suffix(&edit_btn);

            group.add(&row);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row.upcast());
        }

        // 2. Shared Mailboxes
        for profile in shared_profiles {
            let state = if profile.enabled {
                "aktiv"
            } else {
                "disabled"
            };
            let parent_info = if let Some(ref parent_id) = profile.parent_account_id {
                let parent_name = profiles
                    .iter()
                    .find(|p| &p.id == parent_id || parent_id.contains(&p.email))
                    .map(|p| p.display_name.as_str())
                    .unwrap_or(parent_id);
                format!(" · Via: {parent_name}")
            } else {
                String::new()
            };
            let row = adw::ActionRow::builder()
                .title(&profile.display_name)
                .subtitle(format!(
                    "{} · Exchange (EWS) · Shared Mailbox · {state}{parent_info}",
                    profile.email
                ))
                .activatable(true)
                .build();
            let icon_shared = gtk::Image::from_icon_name("system-users-symbolic");
            row.add_prefix(&icon_shared);

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
                .tooltip_text("Edit Shared Mailbox")
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

            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove Shared Mailbox")
                .css_classes(vec!["flat", "error"])
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&delete_button);
            let page = self.downgrade();
            let profile_id = profile.id.clone();
            let profile_name = profile.display_name.clone();
            delete_button.connect_clicked(move |_| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                let page_for_action = page.clone();
                let profile_id = profile_id.clone();
                crate::dialogs::run_destructive_action(
                    &page,
                    &format!("Remove shared mailbox “{profile_name}”?"),
                    "The shared mailbox will be removed from the account list and sidebar. The mailbox on the Exchange server remains untouched.",
                    "Remove",
                    move || match AccountConfigStore::default().delete(&profile_id) {
                        Ok(true) => {
                            page_for_action.reconcile_local_default(&profile_id);
                            page_for_action.refresh_managed_account_rows();
                            page_for_action.refresh_local_profile_rows();
                            page_for_action.show_toast("Shared mailbox removed");
                            if let Some(window) = crate::window::find_meridian_window(&page_for_action) {
                                window.reload_accounts();
                            }
                        }
                        Ok(false) => {
                            page_for_action.show_toast("Mailbox not found")
                        }
                        Err(error) => page_for_action.show_toast(&format!(
                            "Could not remove mailbox: {error}"
                        )),
                    },
                );
            });

            group.add(&row);
            self.imp()
                .managed_account_rows
                .borrow_mut()
                .push(row.upcast());
        }
    }

    fn prompt_open_shared_mailbox(&self) {
        let root_window = self.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let window = adw::Window::builder()
            .title("Open Shared Mailbox")
            .modal(true)
            .default_width(520)
            .default_height(400)
            .build();
        if let Some(w) = root_window.as_ref() {
            window.set_transient_for(Some(w));
        }

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let cancel_btn = gtk::Button::builder().label("Cancel").build();
        let add_btn = gtk::Button::builder()
            .label("Add")
            .css_classes(vec!["suggested-action"])
            .sensitive(false)
            .build();
        header.pack_start(&cancel_btn);
        header.pack_end(&add_btn);
        toolbar.add_top_bar(&header);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

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
            if matches!(p.protocol, AccountProtocol::Exchange) && !p.is_shared_mailbox {
                candidates.push(ParentCandidate {
                    id: p.id.clone(),
                    display: format!("{} ({})", p.display_name, p.email),
                    username: p.username.clone(),
                    imap_host: p.imap_host.clone(),
                    auth_method: p.auth_method.clone(),
                    oab_url: p.oab_url.clone(),
                });
            }
        }
        for s in &eds_sources {
            let is_ews = s.backend_name.to_ascii_lowercase().contains("ews")
                || s.backend_name.to_ascii_lowercase().contains("exchange");
            if is_ews && !candidates.iter().any(|c| c.id == s.uid || c.display.contains(&s.display_name)) {
                candidates.push(ParentCandidate {
                    id: s.uid.clone(),
                    display: format!("{} [Exchange]", s.display_name),
                    username: s.display_name.clone(),
                    imap_host: String::new(),
                    auth_method: "NTLM".to_string(),
                    oab_url: String::new(),
                });
            }
        }

        let combo_parent = if !candidates.is_empty() {
            let labels: Vec<String> = candidates.iter().map(|c| c.display.clone()).collect();
            let label_slices: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let string_list = gtk::StringList::new(&label_slices);
            let combo = adw::ComboRow::builder()
                .title("Primary Exchange Account")
                .subtitle("Account with delegate permission")
                .model(&string_list)
                .selected(0)
                .build();
            group.add(&combo);
            Some(combo)
        } else {
            let row_no_ews = adw::ActionRow::builder()
                .title("No primary Exchange account found")
                .subtitle("Please configure your primary Exchange account first")
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

        content_box.append(&group);
        toolbar.set_content(Some(&content_box));
        window.set_content(Some(&toolbar));

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

        window.present();
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
                title: "Mail Accounts",
                icon: "avatar-default-symbolic",
            },
            SidePageDef {
                id: "contacts",
                title: "Address Books",
                icon: "x-office-address-book-symbolic",
            },
            SidePageDef {
                id: "labels",
                title: "Colors and Labels",
                icon: "color-select-symbolic",
            },
            SidePageDef {
                id: "certificates",
                title: "Certificates & PGP",
                icon: "security-high-symbolic",
            },
        ],
    },
    SideSectionDef {
        section: "Settings",
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
                title: "Message List & Layout",
                icon: "view-list-symbolic",
            },
            SidePageDef {
                id: "mail_reading",
                title: "Reading & Display",
                icon: "mail-read-symbolic",
            },
            SidePageDef {
                id: "composer",
                title: "Composing",
                icon: "document-edit-symbolic",
            },
            SidePageDef {
                id: "replies",
                title: "Replies & Forwarding",
                icon: "mail-reply-sender-symbolic",
            },
            SidePageDef {
                id: "signatures",
                title: "Signatures",
                icon: "accessories-text-editor-symbolic",
            },
            SidePageDef {
                id: "headers",
                title: "Headers",
                icon: "document-properties-symbolic",
            },
            SidePageDef {
                id: "junk",
                title: "Junk & Spam",
                icon: "mail-mark-junk-symbolic",
            },
            SidePageDef {
                id: "calendar",
                title: "Calendar & Tasks",
                icon: "x-office-calendar-symbolic",
            },
            SidePageDef {
                id: "calendar_display",
                title: "Display & Formats",
                icon: "preferences-desktop-display-symbolic",
            },
            SidePageDef {
                id: "network",
                title: "Network & Proxy",
                icon: "network-wired-symbolic",
            },
            SidePageDef {
                id: "backup",
                title: "Import and Backup",
                icon: "drive-harddisk-symbolic",
            },
        ],
    },
];

pub fn get_page_title(id: &str) -> Option<String> {
    for sec in SIDEBAR_SECTIONS {
        for page in sec.pages {
            if page.id == id {
                return Some(gettext(page.title));
            }
        }
    }
    None
}

pub fn select_sidebar_page(sidebar_list: &gtk::ListBox, page_id: &str) {
    let target_name = format!("page:{page_id}");
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

impl PreferencesDialog {
    fn build_ui(&self) {
        let settings = AppSettings::try_new();

        // ── 9 Sidebar Pages matching Original Evolution ───────────────────────
        let page_accounts = adw::PreferencesPage::builder()
            .title("Mail Accounts")
            .icon_name("avatar-default-symbolic")
            .build();

        let page_contacts = adw::PreferencesPage::builder()
            .title("Contacts")
            .icon_name("x-office-address-book-symbolic")
            .build();

        let page_network = adw::PreferencesPage::builder()
            .title("Network Preferences")
            .icon_name("network-wired-symbolic")
            .build();

        let page_backup = adw::PreferencesPage::builder()
            .title("Import and Backup")
            .icon_name("drive-harddisk-symbolic")
            .build();

        let group_accounts = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Email Accounts"))
            .description(&crate::i18n::gettext("Manage accounts and online services"))
            .build();

        let row_add_account = adw::ActionRow::builder()
            .title("Add New Account…")
            .subtitle("Set up IMAP, POP3, Exchange (EWS), or an online account")
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

        let row_add_shared = adw::ActionRow::builder()
            .title("Open Shared Mailbox…")
            .subtitle(&crate::i18n::gettext(
                "Connect shared mailbox (Exchange EWS) without dedicated password",
            ))
            .activatable(true)
            .build();
        let icon_add_shared = gtk::Image::from_icon_name("system-users-symbolic");
        row_add_shared.add_prefix(&icon_add_shared);

        row_add_shared.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                page.prompt_open_shared_mailbox();
            }
        ));
        group_accounts.add(&row_add_shared);

        let row_oof = adw::ActionRow::builder()
            .title(&crate::i18n::gettext("Out of Office (Auto-Responder)…"))
            .subtitle(&crate::i18n::gettext("Set up out of office and automatic replies"))
            .activatable(true)
            .build();
        let icon_oof = gtk::Image::from_icon_name("mail-outbox-symbolic");
        row_oof.add_prefix(&icon_oof);

        row_oof.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::OutOfOfficeDialog::new();
                dialog.present(Some(&page));
            }
        ));
        group_accounts.add(&row_oof);

        self.imp()
            .managed_accounts_group
            .replace(Some(group_accounts.clone()));
        self.refresh_managed_account_rows();

        page_accounts.add(&group_accounts);

        let group_profiles = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Configured Accounts"))
            .description(
                &crate::i18n::gettext(
                    "Manage default account, email identities, and display order",
                ),
            )
            .build();
        self.imp()
            .local_profile_group
            .replace(Some(group_profiles.clone()));
        self.refresh_local_profile_rows();
        page_accounts.add(&group_profiles);


        // Filter Rules Section
        let group_filters = adw::PreferencesGroup::builder()
            .title("Mail Filters and Sorting")
            .description(
                "Automatic rules for incoming mail and manual execution on local store",
            )
            .build();

        let row_add_filter = adw::ActionRow::builder()
            .title("Create New Filter Rule…")
            .subtitle("Set conditions (subject, sender, destination folder)")
            .activatable(true)
            .build();
        let icon_filter = gtk::Image::from_icon_name("funnel-symbolic");
        row_add_filter.add_prefix(&icon_filter);

        row_add_filter.connect_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let dialog = crate::dialogs::FilterEditorDialog::new();
                dialog.present(Some(&page));
            }
        ));
        group_filters.add(&row_add_filter);

        let row_test_all = adw::ActionRow::builder()
            .title("Dry run for all active filter rules…")
            .subtitle("Simulation on local mail store without applying changes")
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
        group_filters.add(&row_test_all);

        let row_log_actions = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Log filter actions"))
            .subtitle(&crate::i18n::gettext(
                "Writes executed filter actions and errors to the filter log file",
            ))
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
            .title("Filter Log File")
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
            group_filters.add(&row);
        }
        page_accounts.add(&group_filters);

        let group_sync = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Mail Synchronization"))
            .description(&crate::i18n::gettext("Inbox update schedule and outbox delivery"))
            .build();

        let sync_interval_model = gtk::StringList::new(&[]);
        for opt in [
            crate::i18n::gettext("Manually"),
            crate::i18n::gettext("Every minute"),
            crate::i18n::gettext("Every 5 minutes (Default)"),
            crate::i18n::gettext("Every 10 minutes"),
            crate::i18n::gettext("Every 15 minutes"),
            crate::i18n::gettext("Every 30 minutes"),
            crate::i18n::gettext("Hourly"),
            crate::i18n::gettext("Daily"),
        ] {
            sync_interval_model.append(&opt);
        }
        let row_sync_interval = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Check for new messages"))
            .subtitle(&crate::i18n::gettext("Automatic email synchronization interval"))
            .model(&sync_interval_model)
            .build();
        row_sync_interval.add_prefix(&gtk::Image::from_icon_name("mail-receive-symbolic"));

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

        let row_flush_outbox = adw::SwitchRow::builder()
            .title("Empty outbox when checking for mail")
            .subtitle("Automatically sends queued messages in outbox when checking for new mail")
            .build();
        if let Some(settings) = settings.clone() {
            row_flush_outbox.set_active(settings.flush_outbox());
            let settings_for_flush = settings.clone();
            row_flush_outbox.connect_active_notify(move |switch| {
                settings_for_flush.set_flush_outbox(switch.is_active());
            });
        }
        group_sync.add(&row_flush_outbox);

        let row_sync_message_limit = adw::SpinRow::with_range(25.0, 1_000.0, 25.0);
        row_sync_message_limit.set_title("Messages to fetch per folder");
        row_sync_message_limit.set_subtitle(
            "Messages per fetch page; additional messages can be loaded with \"Load More\"",
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

        let group_network_options = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Network Connection"))
            .description(&crate::i18n::gettext("Offline connection settings and local mail store"))
            .build();

        let row_offline_mode = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Offline Mode"))
            .subtitle(&crate::i18n::gettext(
                "Uses local store exclusively and pauses account synchronization",
            ))
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
        group_network_options.add(&row_offline_mode);

        let row_local_mail = directory_picker_row(
            self,
            "Local mail folder",
            "Automatically detected Evolution folder",
            settings
                .as_ref()
                .map(AppSettings::local_mail_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_mail_directory(path),
        );
        group_network_options.add(&row_local_mail);
        page_network.add(&group_network_options);

        let group_calendar_local = adw::PreferencesGroup::builder()
            .title("Local calendar sources and import")
            .description("Local iCalendar storage folder and import of external calendar files")
            .build();
        let row_local_calendar = directory_picker_row(
            self,
            "Local calendar folder",
            "Automatically detected iCalendar folder",
            settings
                .as_ref()
                .map(AppSettings::local_calendar_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_calendar_directory(path),
        );
        group_calendar_local.add(&row_local_calendar);

        let group_contacts_local = adw::PreferencesGroup::builder()
            .title("Local contacts and import")
            .description("Local vCard storage folder and import of external address data")
            .build();
        let row_local_contacts = directory_picker_row(
            self,
            "Local contacts folder",
            "Automatically detected vCard folder",
            settings
                .as_ref()
                .map(AppSettings::local_contact_directory)
                .unwrap_or_default(),
            settings.clone(),
            |settings, path| settings.set_local_contact_directory(path),
        );
        group_contacts_local.add(&row_local_contacts);

        let row_import_calendar = adw::ActionRow::builder()
            .title("Import iCalendar Files")
            .subtitle("Copy ICS/VCS files into local calendar without overwriting")
            .build();
        let import_calendar_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Select iCalendar Files")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_calendar.add_suffix(&import_calendar_button);
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
        group_calendar_local.add(&row_import_calendar);

        let row_import_contacts = adw::ActionRow::builder()
            .title("Import vCard Files")
            .subtitle("Copy VCF/vCard files into local address book without overwriting")
            .build();
        let import_contacts_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Select vCard Files")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_contacts.add_suffix(&import_contacts_button);
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
        group_contacts_local.add(&row_import_contacts);
        page_contacts.add(&group_contacts_local);


        let group_default_address_book = adw::PreferencesGroup::builder()
            .title("Standardadressbuch")
            .description("New contacts and local contact search use this source")
            .build();
        group_default_address_book.add(&source_preference_row(
            "Standardadressbuch",
            "Synchronized and local address books",
            EdsSourceKind::AddressBook,
            "local-addressbook",
            "Lokales Adressbuch",
            settings.clone(),
            AppSettings::default_address_book_source_uid,
            AppSettings::set_default_address_book_source_uid,
        ));
        page_contacts.add(&group_default_address_book);

        let group_contacts_display = adw::PreferencesGroup::builder()
            .title("Appearance and Maps")
            .description("Address formatting, display order, and map services")
            .build();

        let row_address_formatting = adw::SwitchRow::builder()
            .title("Adressformatierung")
            .subtitle("Structure addresses in contact view with line breaks")
            .build();

        let row_home_before_work = adw::SwitchRow::builder()
            .title("Home address before work address")
            .subtitle("Prioritize private contact details before business in preview")
            .build();

        let row_preview_show_maps = adw::SwitchRow::builder()
            .title("Show map hints")
            .subtitle("Signal map availability in contact preview")
            .build();

        let row_open_map_target = adw::ComboRow::builder()
            .title("Kartendienst")
            .subtitle("Service for opening addresses on maps")
            .model(&gtk::StringList::new(&["OpenStreetMap", "Google Maps"]))
            .build();

        let row_open_map_prefer_local = adw::SwitchRow::builder()
            .title("Prefer local map application")
            .subtitle("Prefer system geo: map apps over web browser")
            .build();

        let row_contact_preview_visible = adw::SwitchRow::builder()
            .title("Show contact preview")
            .subtitle("Show preview pane for selected contacts")
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
            .title("Autocompletion")
            .description("Settings for address and contact search when composing")
            .build();

        let row_completion_show_address = adw::SwitchRow::builder()
            .title("Show email address")
            .subtitle("Show email address next to name in suggestions")
            .build();

        let row_completion_min_query_length = adw::SpinRow::with_range(1.0, 100.0, 1.0);
        row_completion_min_query_length.set_title("Minimum search length");
        row_completion_min_query_length
            .set_subtitle("Minimum character count before address search begins");
        row_completion_min_query_length.set_digits(0);
        row_completion_min_query_length.set_snap_to_ticks(true);

        let row_completion_book_sources = adw::EntryRow::builder()
            .title("Address book sources for autocompletion")
            .show_apply_button(true)
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

        let group_backup = adw::PreferencesGroup::builder()
            .title("Data and Backup")
            .description(
                "Back up, diagnose, migrate, or delete app-local state; external profiles remain untouched",
            )
            .build();
        let row_backup = adw::ActionRow::builder()
            .title("Back Up App Data")
            .subtitle("Drafts, filters, local state, and account profiles without secrets")
            .build();
        let backup_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Create Backup")
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
                            Ok(_) => dialog.show_toast("App data backed up"),
                            Err(error) => dialog.show_toast(&format!(
                                "Backup could not be saved: {error}"
                            )),
                        }
                    }
                });
            }
        ));
        group_backup.add(&row_backup);

        let row_restore = adw::ActionRow::builder()
            .title("Restore App Data")
            .subtitle("Replaces only app-owned files included in the backup")
            .build();
        let restore_button = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Restore Backup")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_restore.add_suffix(&restore_button);
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
        group_backup.add(&row_restore);

        let row_diagnostic = adw::ActionRow::builder()
            .title("Create Diagnostic Bundle")
            .subtitle(
                "Technical metadata and file sizes only; no message contents or secrets",
            )
            .build();
        let diagnostic_button = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save Diagnostic Bundle")
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
        group_backup.add(&row_diagnostic);

        let row_delete = adw::ActionRow::builder()
            .title("Delete application data")
            .subtitle(
                "Remove account profiles, drafts, filters, local app stores, and settings",
            )
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete application data")
            .css_classes(vec!["flat", "error"])
            .valign(gtk::Align::Center)
            .build();
        row_delete.add_suffix(&delete_button);
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
        group_backup.add(&row_delete);

        let row_import_profile = adw::ActionRow::builder()
            .title("Evolution-Profil importieren")
            .subtitle("Import non-secret account metadata and valid local iCalendar/vCard files from a selected profile copy")
            .build();
        let import_profile_button = gtk::Button::builder()
            .icon_name("document-import-symbolic")
            .tooltip_text("Select Evolution profile folder")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_profile.add_suffix(&import_profile_button);
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
        group_backup.add(&row_import_profile);

        let row_import_mail = adw::ActionRow::builder()
            .title("Import Local Mail Files")
            .subtitle("Copy EML, mbox, or Maildir files/folders into configured local import folder")
            .build();
        let import_mail_button = gtk::Button::builder()
            .icon_name("mail-inbox-symbolic")
            .tooltip_text("Select mail files")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_import_mail.add_suffix(&import_mail_button);
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
        group_backup.add(&row_import_mail);
        page_backup.add(&group_backup);

        let group_composer = adw::PreferencesGroup::builder()
            .title("Composer")
            .description("Format, signature, and templates for new messages")
            .build();
        let row_composer_html = adw::SwitchRow::builder()
            .title("Use HTML when composing")
            .subtitle("Disable to compose new messages as plain text by default")
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
            .title("Request read receipt")
            .subtitle(
                "Adds RFC-compliant read receipt request to new messages",
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

        let row_composer_inline_spelling = adw::SwitchRow::builder()
            .title("Inline spell checking")
            .subtitle("Enable spell checking while typing in composer")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            row_composer_inline_spelling.set_active(settings.composer_inline_spelling());
            row_composer_inline_spelling.connect_active_notify(move |row| {
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

        let row_spellcheck_languages = adw::ComboRow::builder()
            .title("Spell check languages")
            .subtitle(
                "Dictionary languages for new composers (system default, German, or English)",
            )
            .model(&gtk::StringList::new(&[
                "Systemstandard",
                "Deutsch",
                "Englisch",
                "German and English",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let updating = std::rc::Rc::new(std::cell::Cell::new(false));
            let updating_notify = updating.clone();
            let settings_notify = settings.clone();

            let parse_lang_selection = |lang_str: &str| -> u32 {
                match lang_str {
                    "de-DE" | "de_DE" => 1,
                    "en-US" | "en_US" => 2,
                    "de-DE,en-US" | "de_DE,en_US" => 3,
                    _ => 0,
                }
            };

            row_spellcheck_languages.set_selected(parse_lang_selection(&settings.spellcheck_languages()));
            row_spellcheck_languages.connect_selected_notify(move |row| {
                if updating_notify.get() {
                    return;
                }
                settings_notify.set_spellcheck_languages(match row.selected() {
                    1 => "de-DE",
                    2 => "en-US",
                    3 => "de-DE,en-US",
                    _ => "",
                });
            });

            let row_for_change = row_spellcheck_languages.clone();
            let updating_change = updating.clone();
            settings.connect_changed(Some("spellcheck-languages"), move |s, _| {
                let target = parse_lang_selection(&s.string("spellcheck-languages"));
                if row_for_change.selected() != target {
                    updating_change.set(true);
                    row_for_change.set_selected(target);
                    updating_change.set(false);
                }
            });
        }
        group_composer.add(&row_spellcheck_languages);

        let row_composer_charset = adw::ComboRow::builder()
            .title("Default Character Set")
            .subtitle("Default character encoding for newly composed emails")
            .model(&gtk::StringList::new(&[
                "UTF-8 (Empfohlen)",
                "ISO-8859-1 (Latin-1)",
                "ISO-8859-15 (Latin-9)",
                "Windows-1252 (Western European)",
                "US-ASCII (Klartext)",
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
        group_composer.add(&row_composer_charset);

        let row_composer_draft_autosave = adw::SpinRow::with_range(0.0, 3_600.0, 5.0);
        row_composer_draft_autosave.set_title("Automatically save drafts");
        row_composer_draft_autosave.set_subtitle("seconds; 0 disables local autosave");
        row_composer_draft_autosave.set_digits(0);
        row_composer_draft_autosave.set_snap_to_ticks(true);
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

        let row_append_signature = adw::SwitchRow::builder()
            .title("Automatically insert signature")
            .subtitle("Inserted as plain text or HTML with images at end of new messages")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_append_signature.set_active(settings.append_signature());
            row_append_signature.connect_active_notify(move |row| {
                settings.set_append_signature(row.is_active());
            });
        }

        let row_manage_signatures = adw::ActionRow::builder()
            .title(&gettext("Signaturen verwalten…"))
            .subtitle(&gettext("Formatierte Signaturen mit Logos, Links und Bildern bearbeiten"))
            .activatable(true)
            .build();
        row_manage_signatures.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        group_composer.add(&row_manage_signatures);

        let group_composer_headers = adw::PreferencesGroup::builder()
            .title("Headers Visible by Default")
            .description("Choose which recipient and sender fields are visible by default in composer")
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
        group_composer_headers.add(&row_show_from);

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
        group_composer_headers.add(&row_show_reply_to);

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
        group_composer_headers.add(&row_show_cc);

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
        group_composer_headers.add(&row_show_bcc);

        let group_composer_prompts = adw::PreferencesGroup::builder()
            .title("Send Confirmations")
            .description("Security prompts before sending a message")
            .build();
        let row_prompt_empty_subject = adw::SwitchRow::builder()
            .title("Warn on empty subject")
            .subtitle("Confirm before sending if subject is empty")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_prompt_empty_subject.set_active(settings.composer_prompt_empty_subject());
            row_prompt_empty_subject.connect_active_notify(move |row| {
                settings.set_composer_prompt_empty_subject(row.is_active());
            });
        }
        group_composer_prompts.add(&row_prompt_empty_subject);

        let row_prompt_only_bcc = adw::SwitchRow::builder()
            .title("Warn when only Bcc recipients")
            .subtitle("Confirm before sending if only hidden recipients are specified")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_prompt_only_bcc.set_active(settings.composer_prompt_only_bcc());
            row_prompt_only_bcc.connect_active_notify(move |row| {
                settings.set_composer_prompt_only_bcc(row.is_active());
            });
        }
        group_composer_prompts.add(&row_prompt_only_bcc);

        let group_signature_options = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Signature Options"))
            .description(&crate::i18n::gettext("Signature Placement and Formatting"))
            .build();

        let signature_store = SignatureStore::from_environment();
        let initial_signatures = signature_store.load();
        let initial_sig_names: Vec<&str> = initial_signatures.iter().map(|s| s.name.as_str()).collect();
        let signature_model = gtk::StringList::new(&initial_sig_names);
        let signature_row = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("Active Signature"))
            .subtitle(&crate::i18n::gettext("Standard-Signatur für ausgehende Nachrichten"))
            .model(&signature_model)
            .build();
        if let Some(active_id) = signature_store.active_id() {
            if let Some(index) = initial_signatures
                .iter()
                .position(|signature| signature.id == active_id)
            {
                signature_row.set_selected(index as u32);
            }
        }
        group_signature_options.add(&signature_row);
        group_signature_options.add(&row_append_signature);

        let row_top_signature = adw::SwitchRow::builder()
            .title("Insert signature above quote")
            .subtitle("Place signature above quoted text on replies and forwards (top posting)")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_top_signature.set_active(settings.composer_top_signature());
            row_top_signature.connect_active_notify(move |row| {
                settings.set_composer_top_signature(row.is_active());
            });
        }
        group_signature_options.add(&row_top_signature);

        let row_signature_outline = adw::SwitchRow::builder()
            .title("Insert signature separator (-- )")
            .subtitle("Insert standard separator line before signature")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_signature_outline.set_active(settings.composer_signature_outline());
            row_signature_outline.connect_active_notify(move |row| {
                settings.set_composer_signature_outline(row.is_active());
            });
        }
        group_signature_options.add(&row_signature_outline);

        let group_reply = adw::PreferencesGroup::builder()
            .title("Replying and Forwarding")
            .description("Behavior for quoted original text and reply styles")
            .build();
        let row_quote_original = adw::SwitchRow::builder()
            .title("Originaltext zitieren")
            .subtitle(
                "Inserts original message body into replies and forwards",
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
                "Lines with > prefixes",
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

        let row_reply_style = adw::ComboRow::builder()
            .title("Default Reply Style")
            .subtitle("Behavior when replying to received messages")
            .model(&gtk::StringList::new(&[
                "Quoted (Default)",
                "Eingebettet",
                "As attachment",
                "Do not quote",
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
            .title("Default Forward Style")
            .subtitle("Behavior when forwarding messages")
            .model(&gtk::StringList::new(&[
                "Attached (Default)",
                "Eingebettet",
                "Zitiert",
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
            .title("Erscheinungsbild")
            .icon_name("display-brightness-symbolic")
            .build();

        let group_navigation = adw::PreferencesGroup::builder()
            .title("Navigation and Module Buttons")
            .description(
                "Placement of buttons for Mail, Calendar, Contacts, Tasks, and Memos",
            )
            .build();
        let row_switcher_location = adw::ComboRow::builder()
            .title("Module button placement")
            .subtitle("Choice between modern in window titlebar or classic at bottom of left sidebar")
            .model(&gtk::StringList::new(&[
                "Top header bar (Libadwaita)",
                "Bottom left in sidebar (Classic Evolution)",
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
        group_navigation.add(&row_switcher_location);
        page_appearance.add(&group_navigation);

        let group_mail_reading = adw::PreferencesGroup::builder()
            .title("Message Display and Reading")
            .description("Options for opening, reading, and marking emails")
            .build();

        let group_mail_behavior = adw::PreferencesGroup::builder()
            .title("Navigation and Default Folders")
            .description("Keyboard navigation and startup behavior on open")
            .build();

        let group_mail_html = adw::PreferencesGroup::builder()
            .title("HTML Messages and Privacy")
            .description("Security and display options for formatted emails")
            .build();

        let group_app_layout = adw::PreferencesGroup::builder()
            .title("Window and Controls")
            .description("Global confirmations, status bar, and developer options")
            .build();
        let group_mail_window_layout = adw::PreferencesGroup::builder()
            .title("Window and Layout")
            .description("Arrangement of mail views, sidebar, toolbar, and line density")
            .build();

        let row_image_policy = adw::ComboRow::builder()
            .title("Load images in HTML messages")
            .subtitle("Controls automatic loading of external images and web resources")
            .model(&gtk::StringList::new(&[
                "Never (Recommended for privacy)",
                "Only from known contacts",
                "Always",
            ]))
            .build();
        if let Some(settings) = settings.clone() {
            let policy = settings.mail_image_loading_policy();
            row_image_policy.set_selected(match policy.as_str() {
                "sometimes" | "contacts" => 1,
                "always" => 2,
                _ => 0,
            });
            row_image_policy.connect_selected_notify(move |row| {
                let policy_str = match row.selected() {
                    1 => "sometimes",
                    2 => "always",
                    _ => "never",
                };
                settings.set_mail_image_loading_policy(policy_str);
                settings.set_auto_load_remote_images(row.selected() == 2);
            });
        }
        group_mail_html.add(&row_image_policy);

        let row_html_rendering = adw::SwitchRow::builder()
            .title("Render HTML messages")
            .subtitle("Disable for plain text display only")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_html_rendering.set_active(settings.render_html());
            row_html_rendering.connect_active_notify(move |row| {
                settings.set_render_html(row.is_active());
            });
        }
        group_mail_html.add(&row_html_rendering);

        let row_mark_read = adw::SwitchRow::builder()
            .title("Mark messages as read when selected")
            .subtitle("Setting applies to local and synchronized mail folders")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mark_read.set_active(settings.mail_mark_seen());
            row_mark_read.connect_active_notify(move |row| {
                settings.set_mail_mark_seen(row.is_active());
                settings.set_mark_read_on_selection(row.is_active());
            });
        }
        group_mail_reading.add(&row_mark_read);

        let row_mail_show_mark_read_actions = adw::SwitchRow::builder()
            .title("Show mark-read actions in toolbar")
            .subtitle("Show buttons for marking as read or unread in mail toolbar")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_show_mark_read_actions.set_active(settings.mail_show_mark_read_actions());
            let settings_for_actions = settings.clone();
            row_mail_show_mark_read_actions.connect_active_notify(move |row| {
                settings_for_actions.set_mail_show_mark_read_actions(row.is_active());
            });
            let row_weak = row_mail_show_mark_read_actions.downgrade();
            settings.connect_changed(Some("mail-show-mark-read-actions"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-mark-read-actions");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_reading.add(&row_mail_show_mark_read_actions);

        let row_caret_mode = adw::SwitchRow::builder()
            .title("Caret-Navigation (F7)")
            .subtitle("Allows moving a visible text caret in the message preview")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_caret_mode.set_active(settings.mail_caret_mode());
            row_caret_mode.connect_active_notify(move |row| {
                settings.set_mail_caret_mode(row.is_active());
            });
        }
        group_mail_behavior.add(&row_caret_mode);

        let row_magic_spacebar = adw::SwitchRow::builder()
            .title("Magische Leertaste")
            .subtitle("Spacebar scrolls down in message or jumps to next unread message at end")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_magic_spacebar.set_active(settings.mail_magic_spacebar());
            row_magic_spacebar.connect_active_notify(move |row| {
                settings.set_mail_magic_spacebar(row.is_active());
            });
        }
        group_mail_behavior.add(&row_magic_spacebar);

        let row_show_real_date = adw::SwitchRow::builder()
            .title("Show full date")
            .subtitle("Always shows exact absolute date instead of relative names (such as \"Today\" or \"Yesterday\")")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_real_date.set_active(settings.mail_show_real_date());
            row_show_real_date.connect_active_notify(move |row| {
                settings.set_mail_show_real_date(row.is_active());
            });
        }
        group_mail_reading.add(&row_show_real_date);

        let row_headers_collapsed = adw::SwitchRow::builder()
            .title("Collapse headers by default")
            .subtitle("Shows reduced header view in standard reading view")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_headers_collapsed.set_active(settings.mail_headers_collapsed());
            row_headers_collapsed.connect_active_notify(move |row| {
                settings.set_mail_headers_collapsed(row.is_active());
            });
        }
        group_mail_reading.add(&row_headers_collapsed);

        let row_message_preview = adw::SwitchRow::builder()
            .title("Show message preview pane")
            .subtitle("Shows selected message next to message list")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_message_preview.set_active(settings.show_message_preview());
            row_message_preview.connect_active_notify(move |row| {
                settings.set_show_message_preview(row.is_active());
            });
        }
        group_mail_reading.add(&row_message_preview);

        let row_mail_security_details = adw::SwitchRow::builder()
            .title("Show security status for normal emails")
            .subtitle(
                "Shows S/MIME and PGP encryption details even for unencrypted messages",
            )
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_security_details.set_active(settings.mail_show_security_details());
            row_mail_security_details.connect_active_notify(move |row| {
                settings.set_mail_show_security_details(row.is_active());
            });
        }
        group_mail_reading.add(&row_mail_security_details);

        let row_mail_show_deleted = adw::SwitchRow::builder()
            .title("Show deleted messages")
            .subtitle("Show deleted messages struck through until expunged")
            .active(false)
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
        group_mail_reading.add(&row_mail_show_deleted);

        let row_mail_default_folder = adw::ComboRow::builder()
            .title("Startup Folder")
            .subtitle("Folder selected first when opening mail view")
            .model(&gtk::StringList::new(&[
                "Inbox",
                "All Messages",
                "Sent",
                "Drafts",
                "Trash",
                "Archive",
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
        group_mail_behavior.add(&row_mail_default_folder);

        let row_mail_layout = adw::ComboRow::builder()
            .title("Mail-Arbeitsweise")
            .subtitle(
                "Modern side-by-side or classic Evolution: folders on left, list above, preview below (applied immediately)",
            )
            .model(&gtk::StringList::new(&[
                "Modern: list and preview side-by-side",
                "Classic: List above preview",
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
        group_mail_window_layout.add(&row_mail_layout);

        let row_task_layout = adw::ComboRow::builder()
            .title("Tasks Layout")
            .subtitle(
                "Three-column view or classic Evolution: task list above task preview (applied immediately)",
            )
            .model(&gtk::StringList::new(&[
                "Three-column view: preview to the right of the list",
                "Classic: List above preview",
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
        group_app_layout.add(&row_task_layout);

        let row_memo_layout = adw::ComboRow::builder()
            .title("Memos Layout")
            .subtitle(
                "Three-column view or classic Evolution: memo list above memo preview (applied immediately)",
            )
            .model(&gtk::StringList::new(&[
                "Three-column view: preview to the right of the list",
                "Classic: List above preview",
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
        group_app_layout.add(&row_memo_layout);

        let row_mail_toolbar = adw::SwitchRow::builder()
            .title("Show classic mail toolbar")
            .subtitle(
                "Shows familiar Evolution actions for new mail, reply, archive, and delete above the mail area",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_toolbar.set_active(settings.mail_show_toolbar());
            row_mail_toolbar.connect_active_notify(move |row| {
                settings.set_mail_show_toolbar(row.is_active());
            });
        }
        group_mail_window_layout.add(&row_mail_toolbar);

        let row_mail_statusbar = adw::SwitchRow::builder()
            .title("Show status information at bottom")
            .subtitle(
                "Shows account, loading, and connection status exclusively at bottom of window",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_statusbar.set_active(settings.mail_show_statusbar());
            row_mail_statusbar.connect_active_notify(move |row| {
                settings.set_mail_show_statusbar(row.is_active());
            });
        }
        group_app_layout.add(&row_mail_statusbar);

        let row_debug_console = adw::SwitchRow::builder()
            .title("Enable developer log console")
            .subtitle(
                "Allows expanding a live log console via the status bar for error diagnosis",
            )
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_debug_console.set_active(settings.debug_console_enabled());
            row_debug_console.connect_active_notify(move |row| {
                settings.set_debug_console_enabled(row.is_active());
            });
        }
        group_app_layout.add(&row_debug_console);

        let row_mail_sidebar = adw::SwitchRow::builder()
            .title("Show folder sidebar")
            .subtitle(
                "Keep classic Evolution folder view visible to the left of message list and preview",
            )
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_sidebar.set_active(settings.mail_show_sidebar());
            row_mail_sidebar.connect_active_notify(move |row| {
                settings.set_mail_show_sidebar(row.is_active());
            });
        }
        group_mail_window_layout.add(&row_mail_sidebar);

        let row_mail_density = adw::ComboRow::builder()
            .title("Message list density")
            .subtitle("Determines the vertical spacing of message rows")
            .model(&gtk::StringList::new(&[
                "Kompakt",
                "Komfortabel",
                "Spacious",
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
        group_mail_window_layout.add(&row_mail_density);

        let row_dark_mail = adw::SwitchRow::builder()
            .title("Adapt emails to dark style")
            .subtitle("Shows emails in dark mode when dark system theme is active")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_dark_mail.set_active(settings.mail_dark_mode());
            row_dark_mail.connect_active_notify(move |row| {
                settings.set_mail_dark_mode(row.is_active());
            });
        }
        group_mail_html.add(&row_dark_mail);

        let group_confirmations = adw::PreferencesGroup::builder()
            .title("Security Prompts and Confirmations")
            .description("Behavior for potentially destructive or irreversible user actions")
            .build();
        let row_confirm_destructive = adw::SwitchRow::builder()
            .title("Confirm before destructive actions")
            .subtitle(
                "Prompts before deleting messages, events, tasks, contacts, and memos",
            )
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
        group_confirmations.add(&row_confirm_destructive);
        page_appearance.add(&group_confirmations);

        page_appearance.add(&group_app_layout);

        let group_citations = adw::PreferencesGroup::builder()
            .title("Citations and Highlighting")
            .description("Colors and formatting for nested citation levels in emails")
            .build();

        let row_citation_highlighting = adw::SwitchRow::builder()
            .title("Zitate farblich hervorheben")
            .subtitle("Shows colored left margins for respective citation levels")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_citation_highlighting.set_active(settings.mail_citation_highlighting());
            row_citation_highlighting.connect_active_notify(move |row| {
                settings.set_mail_citation_highlighting(row.is_active());
            });
        }
        group_citations.add(&row_citation_highlighting);

        let group_theme = adw::PreferencesGroup::builder()
            .title("Farbschema")
            .description("System setting or fixed light/dark style")
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
        group_theme.add(&row_color_scheme);
        page_appearance.add(&group_theme);

        let group_mail_list = adw::PreferencesGroup::builder()
            .title("Mail List")
            .description("Sorting of messages and conversations")
            .build();
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
        group_mail_list.add(&row_mail_sort);

        let row_mail_sort_direction = adw::SwitchRow::builder()
            .title("Sort Descending")
            .subtitle("Newest or alphabetically last entries first")
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
            .title("Group Conversations")
            .subtitle("Show reply threads as collapsible trees")
            .active(true)
            .build();
        if let Some(settings) = settings.clone() {
            row_mail_threading.set_active(settings.mail_threading());
            row_mail_threading.connect_active_notify(move |row| {
                settings.set_mail_threading(row.is_active());
            });
        }
        group_mail_list.add(&row_mail_threading);

        let row_mail_date_format = adw::ComboRow::builder()
            .title("Date Format")
            .subtitle("Display of received date in message list")
            .model(&gtk::StringList::new(&[
                "Standard (d Month / Prior year: dd.mm.yy)",
                "Smart (Today: Time, This year: d Month, Prior year: dd.mm.yy)",
                "Short date (dd.mm.yy, e.g. 10.09.26)",
                "Full date (dd.mm.yyyy, e.g. 10.09.2026)",
                "Date & Time (dd.mm.yyyy HH:MM)",
                "ISO 8601 (YYYY-MM-DD)",
                "Custom...",
            ]))
            .build();

        let row_mail_date_custom = adw::EntryRow::builder()
            .title("Custom Date Format (Strftime)")
            .show_apply_button(true)
            .build();

        let row_mail_date_align = adw::ComboRow::builder()
            .title("Date Alignment")
            .subtitle("Date column alignment")
            .model(&gtk::StringList::new(&[
                "Right aligned (Default / Google style)",
                "Left aligned (Classic Evolution)",
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

            row_mail_date_align.set_selected(if settings.mail_date_align() == "left" { 1 } else { 0 });
            let s_align = settings.clone();
            row_mail_date_align.connect_selected_notify(move |row| {
                s_align.set_mail_date_align(if row.selected() == 1 { "left" } else { "right" });
            });
        }
        group_mail_list.add(&row_mail_date_format);
        group_mail_list.add(&row_mail_date_custom);
        group_mail_list.add(&row_mail_date_align);

        let group_mail_headers_mode = adw::PreferencesGroup::builder()
            .title("Header Display")
            .description("Toggle between standard headers and full RFC822 headers")
            .build();
        let row_show_all_headers = adw::SwitchRow::builder()
            .title("Show all headers")
            .subtitle("Show full RFC822 header block in email preview")
            .active(false)
            .build();
        if let Some(settings) = settings.clone() {
            row_show_all_headers.set_active(settings.mail_show_all_headers());
            let settings_for_headers = settings.clone();
            row_show_all_headers.connect_active_notify(move |row| {
                settings_for_headers.set_mail_show_all_headers(row.is_active());
            });
            let row_weak = row_show_all_headers.downgrade();
            settings.connect_changed(Some("mail-show-all-headers"), move |s, _| {
                let Some(row) = row_weak.upgrade() else { return; };
                let active = s.boolean("mail-show-all-headers");
                if row.is_active() != active {
                    row.set_active(active);
                }
            });
        }
        group_mail_headers_mode.add(&row_show_all_headers);

        let row_header_style = adw::ComboRow::builder()
            .title("Header Style")
            .subtitle("Header style in standard email reading view")
            .model(&gtk::StringList::new(&[
                "Modern (Webmail style)",
                "Classic (Tabular)",
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

        let group_mail_normal_headers = adw::PreferencesGroup::builder()
            .title("Standard Message Headers")
            .description(
                "Determines order and visibility of headers in standard reading view. Disabled fields remain available here.",
            )
            .build();
        let normal_headers_editor = build_mail_normal_headers_editor(settings.clone());
        group_mail_normal_headers.add(&normal_headers_editor);

        let group_calendar_view = adw::PreferencesGroup::builder()
            .title("Calendar View")
            .description("Selection is used the next time the calendar view opens")
            .build();
        let row_calendar_view = adw::ComboRow::builder()
            .title("Default View")
            .model(&gtk::StringList::new(&[
                "Day",
                "Week",
                "Work Week",
                "Month",
                "Agenda",
                "Year",
                "List",
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
            .title("Time Divisions")
            .subtitle("Timeline interval in day and work week views")
            .model(&gtk::StringList::new(&[
                "60 Minutes",
                "30 Minutes",
                "15 Minutes",
                "10 Minutes",
                "5 Minutes",
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

        let row_calendar_click_action = adw::ComboRow::builder()
            .title("Click on time slot")
            .subtitle("Action when clicking a free area in the calendar grid")
            .model(&gtk::StringList::new(&[
                "Mark time slot & show draft",
                "Immediately open new event editor",
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
        group_calendar_view.add(&row_calendar_click_action);

        let row_week_start = adw::ComboRow::builder()
            .title("Week Start")
            .model(&gtk::StringList::new(&["Monday", "Sunday"]))
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
            .title("Show calendar weeks")
            .subtitle("Show ISO calendar weeks in month grid and week view")
            .build();
        if let Some(settings) = settings.clone() {
            row_week_numbers.set_active(settings.calendar_show_week_numbers());
            row_week_numbers.connect_active_notify(move |row| {
                settings.set_calendar_show_week_numbers(row.is_active());
            });
        }
        group_calendar_view.add(&row_week_numbers);

        let row_compress_weekend = adw::SwitchRow::builder()
            .title("Compress weekend in month view")
            .subtitle("Compress weekend into a single column")
            .build();
        if let Some(settings) = settings.clone() {
            row_compress_weekend.set_active(settings.calendar_compress_weekend());
            row_compress_weekend.connect_active_notify(move |row| {
                settings.set_calendar_compress_weekend(row.is_active());
            });
        }
        group_calendar_view.add(&row_compress_weekend);

        let row_month_scroll_by_week = adw::SwitchRow::builder()
            .title("Scroll Month View by Week")
            .subtitle("Scrolls by one week instead of an entire month")
            .build();
        if let Some(settings) = settings.clone() {
            row_month_scroll_by_week.set_active(settings.calendar_month_scroll_by_week());
            row_month_scroll_by_week.connect_active_notify(move |row| {
                settings.set_calendar_month_scroll_by_week(row.is_active());
            });
        }
        group_calendar_view.add(&row_month_scroll_by_week);

        let row_dim_past_events = adw::SwitchRow::builder()
            .title("Dim past events")
            .subtitle("Display past events semi-transparently")
            .build();
        if let Some(settings) = settings.clone() {
            row_dim_past_events.set_active(settings.calendar_dim_past_events());
            row_dim_past_events.connect_active_notify(move |row| {
                settings.set_calendar_dim_past_events(row.is_active());
            });
        }
        group_calendar_view.add(&row_dim_past_events);

        let row_date_format = adw::ComboRow::builder()
            .title("Date Format")
            .subtitle("Display in calendar overviews and agenda")
            .model(&gtk::StringList::new(&[
                "System default (Locale)",
                "German (e.g. 19.09.2026)",
                "US (z. B. 09/19/2026)",
                "ISO (z. B. 2026-09-19)",
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
        group_calendar_view.add(&row_date_format);

        let row_time_format = adw::ComboRow::builder()
            .title("Time Format")
            .subtitle("Timeline and Event Times")
            .model(&gtk::StringList::new(&[
                "System default (Locale)",
                "24-hour format (e.g. 14:30)",
                "12-hour format (e.g. 2:30 pm)",
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
        group_calendar_view.add(&row_time_format);

        let row_calendar_time_zone = adw::EntryRow::builder().title("Calendar time zone").build();
        row_calendar_time_zone.set_tooltip_text(Some(
            "system for machine time zone, UTC, or an IANA zone such as Europe/Berlin",
        ));
        if let Some(settings) = settings.clone() {
            row_calendar_time_zone.set_text(&settings.calendar_time_zone());
            row_calendar_time_zone.connect_changed(move |row| {
                settings.set_calendar_time_zone(&row.text());
            });
        }
        group_calendar_view.add(&row_calendar_time_zone);

        let row_second_zone = adw::EntryRow::builder().title("Second Time Zone").build();
        row_second_zone.set_tooltip_text(Some(
            "Additional time zone for day view (e.g. UTC or America/New_York)",
        ));
        if let Some(settings) = settings.clone() {
            row_second_zone.set_text(&settings.calendar_second_zone());
            row_second_zone.connect_changed(move |row| {
                settings.set_calendar_second_zone(&row.text());
            });
        }
        group_calendar_view.add(&row_second_zone);

        let group_default_ecal_sources = adw::PreferencesGroup::builder()
            .title("Default Sources")
            .description("Sources for new events, tasks, and memos")
            .build();
        group_default_ecal_sources.add(&source_preference_row(
            "Default Calendar",
            "New events are created here",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Calendar",
            settings.clone(),
            AppSettings::default_calendar_source_uid,
            AppSettings::set_default_calendar_source_uid,
        ));
        group_default_ecal_sources.add(&source_preference_row(
            "Default Task List",
            "New tasks are created here",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Task List",
            settings.clone(),
            AppSettings::default_task_source_uid,
            AppSettings::set_default_task_source_uid,
        ));
        group_default_ecal_sources.add(&source_preference_row(
            "Default Memo List",
            "New memos are created here",
            EdsSourceKind::Calendar,
            "local-calendar",
            "Local Memo List",
            settings.clone(),
            AppSettings::default_memo_source_uid,
            AppSettings::set_default_memo_source_uid,
        ));

        let group_calendar_days = adw::PreferencesGroup::builder()
            .title("Work Days")
            .description("These weekdays are displayed in the work week view")
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
            group_calendar_days.add(&row);
        }

        let group_calendar_time = adw::PreferencesGroup::builder()
            .title(&crate::i18n::gettext("Working Hours"))
            .description(&crate::i18n::gettext(
                "Daily working hours shown in day and workweek views",
            ))
            .build();

        let workday_options = workday_time_options();
        let row_workday_start = adw::ComboRow::builder()
            .title("Work Day Starts")
            .subtitle(&crate::i18n::gettext("Beginning of the working day"))
            .model(&workday_options)
            .build();
        row_workday_start.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));

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
            .subtitle(&crate::i18n::gettext("End of the working day"))
            .model(&workday_options)
            .build();
        row_workday_end.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));

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
            .description("Default reminders for events and notifications")
            .build();

        let row_use_default_reminder = adw::SwitchRow::builder()
            .title("Default reminder for events")
            .subtitle("Automatically add reminder to new events")
            .build();
        let row_default_reminder_interval = adw::SpinRow::with_range(1.0, 10_080.0, 1.0);
        row_default_reminder_interval.set_title("Default reminder interval");
        row_default_reminder_interval.set_subtitle("Before start of event");
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
            .subtitle("Reminders for contacts with birthday or anniversary")
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
        group_calendar_reminders.add(&row_use_ba_reminder);
        group_calendar_reminders.add(&row_ba_reminder_interval);
        group_calendar_reminders.add(&row_ba_reminder_units);

        let row_hide_cancelled_events = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Hide cancelled events"))
            .subtitle(&crate::i18n::gettext("Do not show events with CANCELLED status in views"))
            .build();
        if let Some(settings) = settings.clone() {
            row_hide_cancelled_events.set_active(settings.hide_cancelled_events());
            let settings_for_cancelled = settings.clone();
            row_hide_cancelled_events.connect_active_notify(move |row| {
                settings_for_cancelled.set_hide_cancelled_events(row.is_active());
            });
        }
        group_calendar_reminders.add(&row_hide_cancelled_events);

        let row_reminder_sound = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Play sound for reminders"))
            .subtitle(&crate::i18n::gettext(
                "Play an audible alert when a calendar or task reminder is due",
            ))
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
        group_calendar_reminders.add(&row_reminder_sound);

        let row_reminder_tray = adw::SwitchRow::builder()
            .title(&crate::i18n::gettext("Show reminders in notification tray only"))
            .subtitle(&crate::i18n::gettext(
                "Display reminders as notifications without automatically opening the dialog window",
            ))
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
            .title(&crate::i18n::gettext("Desktop Notifications"))
            .subtitle(
                &crate::i18n::gettext("Show local event and task reminders as GNOME notifications"),
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
            .title(&crate::i18n::gettext("Calendar Categories"))
            .description(
                &crate::i18n::gettext(
                    "Category names are stored in iCalendar records; colors remain app presentation data",
                ),
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
            .label("Add Category")
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
            .title("Task and Memo Lists")
            .description("Filtering and sorting of task and memo views")
            .build();

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
        group_task_memo_lists.add(&row_task_filter);

        let row_task_sort = adw::ComboRow::builder()
            .title("Sort tasks by")
            .model(&gtk::StringList::new(&["Due Date", "Title", "Priority"]))
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
            .title("Sort tasks descending")
            .subtitle("Later due dates, titles, or higher priorities first")
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
            .title("Hide completed tasks")
            .subtitle("Hide completed tasks after a specified delay")
            .build();
        let row_hide_completed_tasks_value = adw::SpinRow::with_range(0.0, 1000.0, 1.0);
        row_hide_completed_tasks_value.set_title("Delay for completed tasks");
        row_hide_completed_tasks_value.set_subtitle("0 = hide immediately upon completion");
        row_hide_completed_tasks_value.set_digits(0);
        row_hide_completed_tasks_value.set_snap_to_ticks(true);

        let row_hide_completed_tasks_units = adw::ComboRow::builder()
            .title("Unit for completion delay")
            .model(&gtk::StringList::new(&["Minutes", "Hours", "Days"]))
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
            .title("Hide cancelled tasks")
            .subtitle("Do not show tasks with status CANCELLED in views")
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
            .title("Sort memos by")
            .model(&gtk::StringList::new(&[
                "Modification Date",
                "Title",
                "Source",
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
            .title("Sort memos descending")
            .subtitle("Newest modifications, titles, or sources first")
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
            .title("Encryption and Signatures")
            .description("OpenPGP and S/MIME Configuration")
            .build();

        let crypto_protocol_model =
            gtk::StringList::new(&["OpenPGP (GnuPG)", "S/MIME (OpenSSL/CMS)"]);
        let row_crypto_protocol = adw::ComboRow::builder()
            .title("Standardprotokoll")
            .subtitle("Used for new messages and checking incoming messages")
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
            .title("Sign new messages by default")
            .subtitle("Uses selected OpenPGP or S/MIME protocol")
            .active(false)
            .build();
        group_crypto.add(&row_pgp_encrypt);

        let pgp_key_model =
            gtk::StringList::new(&["Automatically choose based on sender address"]);
        let pgp_keys = Rc::new(RefCell::new(Vec::<OpenPgpKey>::new()));
        let row_pgp_key = adw::ComboRow::builder()
            .title("Default PGP Key")
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

        let signer_model = gtk::StringList::new(&["Not set"]);
        let signer_certs: Rc<RefCell<Vec<crate::services::certificate_service::CertificateRecord>>> =
            Rc::new(RefCell::new(Vec::new()));
        let row_smime_signer = adw::ComboRow::builder()
            .title(&crate::i18n::gettext("S/MIME Signature Certificate"))
            .subtitle(&crate::i18n::gettext(
                "PEM or DER certificate for outgoing signatures (from store or file)",
            ))
            .model(&signer_model)
            .build();

        let btn_choose_signer = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Select from external file…")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_smime_signer.add_suffix(&btn_choose_signer);
        group_crypto.add(&row_smime_signer);

        let row_smime_private_key = smime_file_row(
            "S/MIME Private Key",
            "Path to private key file; the key itself is not copied",
            settings
                .as_ref()
                .map(|settings| settings.smime_private_key())
                .unwrap_or_default(),
            settings.clone(),
            AppSettings::set_smime_private_key,
        );
        group_crypto.add(&row_smime_private_key);

        let row_smime_recipients = smime_files_row(
            "S/MIME Recipient Certificates",
            "Certificates for encryption; multiple files can be selected",
            settings
                .as_ref()
                .map(|settings| settings.smime_recipient_certificates())
                .unwrap_or_default(),
            settings.clone(),
            |settings, values| settings.set_smime_recipient_certificates(values),
        );
        group_crypto.add(&row_smime_recipients);

        let ca_model = gtk::StringList::new(&["System Trust Store (Default)"]);
        let ca_certs: Rc<RefCell<Vec<crate::services::certificate_service::CertificateRecord>>> =
            Rc::new(RefCell::new(Vec::new()));
        let row_smime_ca = adw::ComboRow::builder()
            .title("S/MIME-Vertrauensspeicher (optional)")
            .subtitle("CA bundle for trust verification; if omitted, system truststore is used")
            .model(&ca_model)
            .build();

        let btn_choose_ca = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Select from external file…")
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        row_smime_ca.add_suffix(&btn_choose_ca);
        group_crypto.add(&row_smime_ca);

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

        let settings_for_signer_file = settings.clone();
        let row_smime_signer_for_file = row_smime_signer.downgrade();
        let signer_model_for_file = signer_model.clone();
        btn_choose_signer.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else { return; };
            let file_dialog = gtk::FileDialog::builder()
                .title("Select S/MIME Signature Certificate")
                .modal(true)
                .build();
            let settings = settings_for_signer_file.clone();
            let row_weak = row_smime_signer_for_file.clone();
            let model = signer_model_for_file.clone();
            file_dialog.open(Some(&root), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else { return; };
                let Some(path) = file.path() else { return };
                let path_str = path.to_string_lossy().into_owned();
                if let Some(settings) = settings.as_ref() {
                    settings.set_smime_signer_certificate(&path_str);
                }
                let label = format!("Datei: {path_str}");
                model.append(&label);
                if let Some(row) = row_weak.upgrade() {
                    row.set_selected(model.n_items() - 1);
                }
            });
        });

        let settings_for_ca_file = settings.clone();
        let row_smime_ca_for_file = row_smime_ca.downgrade();
        let ca_model_for_file = ca_model.clone();
        btn_choose_ca.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else { return; };
            let file_dialog = gtk::FileDialog::builder()
                .title("Select S/MIME Trust Store (CA)")
                .modal(true)
                .build();
            let settings = settings_for_ca_file.clone();
            let row_weak = row_smime_ca_for_file.clone();
            let model = ca_model_for_file.clone();
            file_dialog.open(Some(&root), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else { return; };
                let Some(path) = file.path() else { return };
                let path_str = path.to_string_lossy().into_owned();
                if let Some(settings) = settings.as_ref() {
                    settings.set_smime_ca_file(&path_str);
                }
                let label = format!("Datei: {path_str}");
                model.append(&label);
                if let Some(row) = row_weak.upgrade() {
                    row.set_selected(model.n_items() - 1);
                }
            });
        });

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
                let mut signer_labels = vec!["Not set".to_string()];
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
                        signer_labels.push(format!("Datei: {current_signer}"));
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
                let mut ca_labels = vec!["System Trust Store (Default)".to_string()];
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
                        ca_labels.push(format!("Datei: {current_ca}"));
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
        let group_cert_management = adw::PreferencesGroup::builder()
            .title("Zertifikatsverwaltung")
            .description("Manage S/MIME and OpenPGP certificates as well as certificate authorities (CA)")
            .build();

        let row_cert_manager = adw::ActionRow::builder()
            .title("Manage certificates and trust…")
            .subtitle("View, import, and export personal certificates, contacts, and certificate authorities")
            .activatable(true)
            .build();
        let cert_mgr_icon = gtk::Image::from_icon_name("security-high-symbolic");
        row_cert_manager.add_prefix(&cert_mgr_icon);

        let btn_cert_manager = gtk::Button::builder()
            .label("Manage Certificates…")
            .icon_name("credentials-symbolic")
            .css_classes(vec!["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        row_cert_manager.add_suffix(&btn_cert_manager);
        group_cert_management.add(&row_cert_manager);

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

        let open_mgr_clone = open_cert_manager.clone();
        btn_cert_manager.connect_clicked(move |_| open_mgr_clone());
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
        page_certificates.add(&group_cert_management);
        page_certificates.add(&group_crypto);

        let group_mail_labels = build_mail_labels_group(&self, settings.clone());
        let group_mail_junk = build_mail_junk_group(settings.clone());
        let group_mail_notifications = build_mail_notifications_group(settings.clone());

        let page_mail_general = adw::PreferencesPage::builder()
            .title(&gettext("General"))
            .icon_name("mail-inbox-symbolic")
            .build();
        page_mail_general.add(&group_sync);
        page_mail_general.add(&group_mail_reading);
        page_mail_general.add(&group_mail_behavior);
        page_mail_general.add(&group_mail_notifications);

        let page_mail_html = adw::PreferencesPage::builder()
            .title(&gettext("Reading & Display"))
            .icon_name("mail-read-symbolic")
            .build();
        page_mail_html.add(&group_mail_html);
        page_mail_html.add(&group_citations);

        let page_mail_labels = adw::PreferencesPage::builder()
            .title(&gettext("Colors and Labels"))
            .icon_name("color-select-symbolic")
            .build();
        page_mail_labels.add(&group_mail_labels);

        let page_mail_headers = adw::PreferencesPage::builder()
            .title(&gettext("Headers"))
            .icon_name("document-properties-symbolic")
            .build();
        page_mail_headers.add(&group_mail_headers_mode);
        page_mail_headers.add(&group_mail_normal_headers);

        let page_mail_junk = adw::PreferencesPage::builder()
            .title(&gettext("Junk & Spam"))
            .icon_name("mail-mark-junk-symbolic")
            .build();
        page_mail_junk.add(&group_mail_junk);

        let page_mail_layout = adw::PreferencesPage::builder()
            .title(&gettext("Message List & Layout"))
            .icon_name("view-list-symbolic")
            .build();
        page_mail_layout.add(&group_mail_window_layout);
        page_mail_layout.add(&group_mail_list);

        let page_composer_general = adw::PreferencesPage::builder()
            .title(&gettext("Composing"))
            .icon_name("document-edit-symbolic")
            .build();
        page_composer_general.add(&group_composer);
        page_composer_general.add(&group_composer_headers);
        page_composer_general.add(&group_composer_prompts);

        let page_composer_replies = adw::PreferencesPage::builder()
            .title(&gettext("Replies & Forwarding"))
            .icon_name("mail-reply-sender-symbolic")
            .build();
        page_composer_replies.add(&group_reply);

        // ── Signatures Detail Box (GNOME HIG Dedicated Page) ────────────────
        let page_signatures = adw::PreferencesPage::builder()
            .title(&gettext("Signaturen"))
            .description(&gettext("Verwalten Sie Ihre E-Mail-Signaturen mit formatiertem Text und Bildern"))
            .build();

        let group_signatures_list = adw::PreferencesGroup::builder()
            .title(&gettext("Meine Signaturen"))
            .description(&gettext("Signaturen für ausgehende E-Mails. Bilder (Logos) werden direkt eingebettet."))
            .build();

        let signatures_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        group_signatures_list.add(&signatures_list_box);
        page_signatures.add(&group_signatures_list);

        page_signatures.add(&group_signature_options);


        // Reactive signature list & selection updater
        let dialog_weak = self.downgrade();
        let signatures_list_box_weak = signatures_list_box.downgrade();
        let signature_row_weak = signature_row.downgrade();

        let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

        let rebuild_fn: Rc<dyn Fn()> = {
            let rebuild_holder_for_rebuild = rebuild_holder.clone();
            Rc::new(move || {
                let Some(list_box) = signatures_list_box_weak.upgrade() else { return };
                let Some(combo) = signature_row_weak.upgrade() else { return };
                let Some(dialog) = dialog_weak.upgrade() else { return };

                while let Some(child) = list_box.first_child() {
                    list_box.remove(&child);
                }

                let store = SignatureStore::from_environment();
                let signatures = store.load();

                if signatures.is_empty() {
                    let empty_row = adw::ActionRow::builder()
                        .title(&gettext("Noch keine Signaturen vorhanden"))
                        .subtitle(&gettext("Erstellen Sie eine neue Signatur, um formatierten Text oder Logos anzulegen."))
                        .build();
                    list_box.append(&empty_row);
                }

                for sig in &signatures {
                    let sig_clone = sig.clone();
                    let sig_id = sig.id.clone();
                    let row = adw::ActionRow::builder()
                        .title(&sig.name)
                        .subtitle(if sig.is_html() {
                            gettext("HTML-Format (unterstützt Bilder und Logos)")
                        } else {
                            gettext("Nur-Text")
                        })
                        .build();
                    row.add_prefix(&gtk::Image::from_icon_name("document-edit-symbolic"));

                    let badge = gtk::Label::builder()
                        .label(if sig.is_html() { "HTML & Bilder" } else { "Text" })
                        .css_classes(vec!["dim-label", "caption"])
                        .valign(gtk::Align::Center)
                        .build();
                    row.add_suffix(&badge);

                    let btn_edit = gtk::Button::builder()
                        .icon_name("document-edit-symbolic")
                        .tooltip_text(&gettext("Signatur im visuellen Editor bearbeiten"))
                        .css_classes(vec!["flat"])
                        .valign(gtk::Align::Center)
                        .build();
                    let dialog_for_edit = dialog.clone();
                    let rebuild_cb = rebuild_holder_for_rebuild.borrow().as_ref().cloned();
                    btn_edit.connect_clicked(move |_| {
                        let cb = rebuild_cb.clone();
                        let editor = SignatureEditorDialog::new_for_edit(&sig_clone, Some(move |_| {
                            if let Some(f) = cb.as_ref() {
                                f();
                            }
                        }));
                        editor.present(Some(&dialog_for_edit));
                    });
                    row.add_suffix(&btn_edit);

                    let btn_delete = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text(&gettext("Signatur löschen"))
                        .css_classes(vec!["flat", "error"])
                        .valign(gtk::Align::Center)
                        .build();
                    let rebuild_cb_del = rebuild_holder_for_rebuild.borrow().as_ref().cloned();
                    let id_for_del = sig_id.clone();
                    btn_delete.connect_clicked(move |_| {
                        let store = SignatureStore::from_environment();
                        let _ = store.remove(&id_for_del);
                        if let Some(f) = rebuild_cb_del.as_ref() {
                            f();
                        }
                    });
                    row.add_suffix(&btn_delete);

                    list_box.append(&row);
                }

                let row_add = adw::ActionRow::builder()
                    .title(&gettext("Neue Signatur erstellen…"))
                    .activatable(true)
                    .build();
                let btn_add = gtk::Button::builder()
                    .icon_name("list-add-symbolic")
                    .css_classes(vec!["suggested-action", "flat"])
                    .valign(gtk::Align::Center)
                    .build();
                let dialog_for_add = dialog.clone();
                let rebuild_cb_add = rebuild_holder_for_rebuild.borrow().as_ref().cloned();
                let open_create = move || {
                    let cb = rebuild_cb_add.clone();
                    let editor = SignatureEditorDialog::new_for_create(Some(move |_| {
                        if let Some(f) = cb.as_ref() {
                            f();
                        }
                    }));
                    editor.present(Some(&dialog_for_add));
                };
                let open_create_rc = Rc::new(open_create);
                let open_create_for_btn = open_create_rc.clone();
                btn_add.connect_clicked(move |_| open_create_for_btn());
                row_add.connect_activated(move |_| open_create_rc());
                row_add.add_suffix(&btn_add);
                list_box.append(&row_add);

            let names: Vec<&str> = signatures.iter().map(|s| s.name.as_str()).collect();
            let model = gtk::StringList::new(&names);
            combo.set_model(Some(&model));
            if let Some(active_id) = store.active_id() {
                if let Some(pos) = signatures.iter().position(|s| s.id == active_id) {
                    combo.set_selected(pos as u32);
                }
            }
            })
        };

        *rebuild_holder.borrow_mut() = Some(rebuild_fn.clone());
        rebuild_fn();

        let settings_for_select = settings.clone();
        let combo_weak = signature_row.downgrade();
        signature_row.connect_selected_notify(move |_| {
            let Some(combo) = combo_weak.upgrade() else { return };
            let store = SignatureStore::from_environment();
            let signatures = store.load();
            let idx = combo.selected() as usize;
            if let Some(sig) = signatures.get(idx) {
                let _ = store.set_active(Some(&sig.id));
                if let Some(settings) = settings_for_select.as_ref() {
                    settings.set_signature_text(&sig.text);
                }
            }
        });

        // ── Calendar and Tasks Pages ─────────────────────────────────────────
        let page_calendar_general = adw::PreferencesPage::builder().title(&gettext("Calendar & Tasks")).build();
        page_calendar_general.add(&group_calendar_view);
        page_calendar_general.add(&group_calendar_days);
        page_calendar_general.add(&group_calendar_time);
        page_calendar_general.add(&group_calendar_reminders);

        let page_calendar_display = adw::PreferencesPage::builder()
            .title(&gettext("Display & Formats"))
            .build();
        page_calendar_display.add(&group_default_ecal_sources);
        page_calendar_display.add(&group_calendar_local);
        page_calendar_display.add(&group_categories);
        page_calendar_display.add(&group_task_memo_lists);

        // ── Detail Stack (17 Flattened Preferences Pages) ───────────────────
        let detail_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .hexpand(true)
            .vexpand(true)
            .build();

        // Accounts
        detail_stack.add_named(&page_accounts, Some("accounts"));
        detail_stack.add_named(&page_contacts, Some("contacts"));

        // Mail
        detail_stack.add_named(&page_mail_general, Some("mail_general"));
        detail_stack.add_named(&page_mail_html, Some("mail_reading"));
        detail_stack.add_named(&page_mail_layout, Some("mail_layout"));
        detail_stack.add_named(&page_composer_general, Some("composer"));
        detail_stack.add_named(&page_composer_replies, Some("replies"));
        detail_stack.add_named(&page_signatures, Some("signatures"));
        detail_stack.add_named(&page_mail_headers, Some("headers"));
        detail_stack.add_named(&page_mail_labels, Some("labels"));
        detail_stack.add_named(&page_mail_junk, Some("junk"));

        // Calendar and Tasks
        detail_stack.add_named(&page_calendar_general, Some("calendar"));
        detail_stack.add_named(&page_calendar_display, Some("calendar_display"));

        // System & Preferences
        detail_stack.add_named(&page_appearance, Some("appearance"));
        detail_stack.add_named(&page_certificates, Some("certificates"));
        detail_stack.add_named(&page_network, Some("network"));
        detail_stack.add_named(&page_backup, Some("backup"));

        // ── Navigation Split View (Modern Libadwaita & GNOME HIG Parity) ────
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(200.0)
            .max_sidebar_width(280.0)
            .sidebar_width_fraction(0.28)
            .build();

        // ── Sidebar Category List & NavigationPage ───────────────────────────
        let sidebar_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar", "settings-side"])
            .build();

        for sec in SIDEBAR_SECTIONS {
            let heading = gtk::ListBoxRow::new();
            heading.set_selectable(false);
            heading.set_activatable(false);
            heading.add_css_class("settings-side-heading");
            let label = gtk::Label::new(Some(&gettext(sec.section)));
            label.set_xalign(0.0);
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            heading.set_child(Some(&label));
            sidebar_list.append(&heading);

            for page in sec.pages {
                let row = gtk::ListBoxRow::new();
                row.set_widget_name(&format!("page:{}", page.id));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                line.set_margin_start(8);
                line.set_margin_end(8);
                line.set_margin_top(6);
                line.set_margin_bottom(6);
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

        // ── Interaction Wiring ──────────────────────────────────────────────
        let detail_stack_weak = detail_stack.downgrade();
        let content_title_weak = content_title.downgrade();
        let content_page_weak = content_page.downgrade();
        let split_weak = split.downgrade();

        sidebar_list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let name = row.widget_name();
            let Some(page_id) = name.strip_prefix("page:") else { return };
            if let Some(stack) = detail_stack_weak.upgrade() {
                stack.set_visible_child_name(page_id);
            }
            let title = get_page_title(page_id).unwrap_or_else(|| page_id.to_string());
            if let Some(ct) = content_title_weak.upgrade() {
                ct.set_title(&title);
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

        results_list.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            let hit = shown_hits_for_pick.borrow().get(idx).cloned();
            let Some(hit) = hit else { return };

            detail_stack_for_pick.set_visible_child_name(hit.page_id);
            let title = get_page_title(hit.page_id).unwrap_or_else(|| hit.page_id.to_string());
            content_title_for_pick.set_title(&title);
            content_page_for_pick.set_title(&title);
            select_sidebar_page(&sidebar_list_for_pick, hit.page_id);
            split_for_pick.set_show_content(true);

            if let Some(target) = hit.widget.upgrade() {
                reveal_widget(&target);
            }
        });

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);

        let search_bar_for_ctrl_f = search_bar.clone();
        let search_entry_for_ctrl_f = search_entry.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                let open = !search_bar_for_ctrl_f.is_search_mode();
                search_bar_for_ctrl_f.set_search_mode(open);
                if open {
                    search_entry_for_ctrl_f.grab_focus();
                }
                glib::Propagation::Stop
            })),
        ));

        let search_bar_for_escape = search_bar.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                if search_bar_for_escape.is_search_mode() {
                    search_bar_for_escape.set_search_mode(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            })),
        ));
        self.add_controller(shortcuts);

        let sidebar_for_nav = sidebar_list.downgrade();
        let split_for_nav = split.downgrade();
        row_manage_signatures.connect_activated(move |_| {
            if let Some(sidebar) = sidebar_for_nav.upgrade() {
                select_sidebar_page(&sidebar, "signatures");
            }
            if let Some(split) = split_for_nav.upgrade() {
                split.set_show_content(true);
            }
        });

        let env_page = std::env::var("MERIDIAN_PREF_PAGE")
            .or_else(|_| std::env::var("EVOLUTION_PREF_PAGE"))
            .ok();
        let env_subpage = std::env::var("MERIDIAN_PREF_SUBPAGE")
            .or_else(|_| std::env::var("EVOLUTION_PREF_SUBPAGE"))
            .ok();

        let initial_page_id = match (env_page.as_deref(), env_subpage.as_deref()) {
            (Some("accounts"), Some("contacts")) => "contacts",
            (Some("accounts"), _) => "accounts",
            (Some("contacts"), _) => "contacts",
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
            (Some("replies"), _) => "replies",
            (Some("signatures"), _) => "signatures",
            (Some("headers"), _) => "headers",
            (Some("labels"), _) => "labels",
            (Some("junk" | "spam"), _) => "junk",
            (Some("calendar"), Some("display")) => "calendar_display",
            (Some("calendar"), _) => "calendar",
            (Some("calendar_display" | "display"), _) => "calendar_display",
            (Some("appearance" | "theme"), _) => "appearance",
            (Some("certificates"), _) => "certificates",
            (Some("network"), _) => "network",
            (Some("backup"), _) => "backup",
            (Some("security"), Some("certificates")) => "certificates",
            (Some("security"), Some("junk")) => "junk",
            (Some("security"), Some("network")) => "network",
            (Some("security"), Some("backup")) => "backup",
            (Some("security"), _) => "backup",
            _ => "mail_general",
        };

        detail_stack.set_visible_child_name(initial_page_id);
        let initial_title = get_page_title(initial_page_id).unwrap_or_else(|| initial_page_id.to_string());
        content_title.set_title(&initial_title);
        content_page.set_title(&initial_title);
        select_sidebar_page(&sidebar_list, initial_page_id);

        self.set_child(Some(&split));
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::MeridianWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
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
        .label("Add")
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
        .label("Reset")
        .tooltip_text("Reset normal headers to Evolution default list")
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

    if let Some(s) = settings.as_ref() {
        let state_changed = Rc::clone(&state);
        let list_changed = list.clone();
        let settings_changed = settings.clone();
        s.connect_changed(Some("mail-normal-headers"), move |_, _| {
            if let Some(app_settings) = settings_changed.as_ref() {
                let current = app_settings.mail_normal_headers();
                if *state_changed.borrow() != current {
                    state_changed.replace(current);
                    rebuild_mail_normal_headers_list(&list_changed, &state_changed, &settings_changed);
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
        .tooltip_text("Show header in standard reading view")
        .build();
    let move_up =
        mail_header_editor_button("go-up-symbolic", "Move header up", index > 0);
    let move_down = mail_header_editor_button(
        "go-down-symbolic",
        "Move header down",
        index + 1 < state.borrow().len(),
    );
    let remove = mail_header_editor_button("user-trash-symbolic", "Remove header", true);

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
            "Not set"
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
        .tooltip_text("Remove path")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear_button);
    let choose_button = gtk::Button::builder()
        .label("Select…")
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
        path_label_for_clear.set_label("Not set");
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    choose_button.connect_clicked(move |button| {
        let Some(root) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("Select S/MIME File")
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
            "Not set"
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
        .tooltip_text("Remove certificate paths")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear_button);
    let choose_button = gtk::Button::builder()
        .label("Select…")
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
        path_label_for_clear.set_label("Not set");
    });

    let settings_for_choose = settings;
    let path_label_for_choose = path_label.clone();
    choose_button.connect_clicked(move |button| {
        let Some(root) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let file_dialog = gtk::FileDialog::builder()
            .title("Select S/MIME Recipient Certificates")
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
    let source_model = gtk::StringList::new(&["Loading sources…"]);
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
                format!("Unavailable source · {preferred_source_uid}"),
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
        .tooltip_text("Select Folder")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text("Reset to automatic selection")
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
            .title("Select local import folder")
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
        .placeholder_text("Name, e.g. Work")
        .hexpand(true)
        .build();
    let color_entry = gtk::Entry::builder()
        .placeholder_text("Color, e.g. #729FCF")
        .width_chars(10)
        .build();
    if let Some(category) = category.as_ref() {
        name_entry.set_text(&category.name);
        color_entry.set_text(&category.color);
    } else {
        color_entry.set_text("#729FCF");
    }
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
        .title("Colors and Labels")
        .description("Color labels for tagging and quick filtering of messages")
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
        .label("Add New Label…")
        .icon_name("list-add-symbolic")
        .css_classes(vec!["suggested-action", "flat"])
        .halign(gtk::Align::Start)
        .build();

    let reset_btn = gtk::Button::builder()
        .label("Reset to default")
        .tooltip_text("Reset labels to Evolution default colors")
        .css_classes(vec!["flat"])
        .halign(gtk::Align::Start)
        .build();

    let action_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(8)
        .build();
    action_box.append(&add_btn);
    action_box.append(&reset_btn);

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
    group.add(&action_box);
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
    row.set_activatable(false);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();

    let color_dot = gtk::Label::builder()
        .label("●")
        .css_classes(vec!["title-2"])
        .valign(gtk::Align::Center)
        .build();
    color_dot.set_markup(&format!(
        "<span foreground=\"{}\" font_size=\"x-large\">●</span>",
        label_def.color
    ));

    let text_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let name_label = gtk::Label::builder()
        .label(&label_def.display_name())
        .halign(gtk::Align::Start)
        .css_classes(vec!["heading"])
        .build();
    let tag_label = gtk::Label::builder()
        .label(format!("Tag: {} · {}", label_def.tag, label_def.color))
        .halign(gtk::Align::Start)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    text_box.append(&name_label);
    text_box.append(&tag_label);

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

    content.append(&color_dot);
    content.append(&text_box);
    content.append(&up_btn);
    content.append(&down_btn);
    content.append(&edit_btn);
    content.append(&delete_btn);
    row.set_child(Some(&content));

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

    let root_window = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());

    let window = adw::Window::builder()
        .title(title)
        .modal(true)
        .default_width(420)
        .default_height(320)
        .build();
    if let Some(w) = root_window.as_ref() {
        window.set_transient_for(Some(w));
    }

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();

    let cancel_btn = gtk::Button::builder().label("Cancel").build();
    let save_btn = gtk::Button::builder()
        .label(if is_edit { "Save" } else { "Add" })
        .css_classes(vec!["suggested-action"])
        .sensitive(!initial_name.trim().is_empty())
        .build();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);
    toolbar.add_top_bar(&header);

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_start(16)
        .margin_end(16)
        .margin_top(16)
        .margin_bottom(16)
        .build();

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
    content_box.append(&group);
    toolbar.set_content(Some(&content_box));
    window.set_content(Some(&toolbar));

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

    window.present();
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
        .description(&crate::i18n::gettext(
            "Automatic detection and handling of unwanted spam mail",
        ))
        .build();

    let bogo_path = detect_program_path("bogofilter");
    let spam_path = detect_program_path("spamassassin");
    let any_available = bogo_path.is_some() || spam_path.is_some();

    let row_check_incoming = adw::SwitchRow::builder()
        .title("Automatically check incoming mail for junk")
        .subtitle("Checks new incoming messages using the selected spam filter engine")
        .active(true)
        .build();
    if let Some(s) = settings.clone() {
        row_check_incoming.set_active(s.junk_check_incoming());
    }
    group.add(&row_check_incoming);

    let row_lookup_ab = adw::SwitchRow::builder()
        .title("Do not mark sender in address books as junk")
        .subtitle("Messages from known contacts are never marked as spam")
        .active(false)
        .build();
    if let Some(s) = settings.clone() {
        row_lookup_ab.set_active(s.junk_lookup_addressbook());
    }
    group.add(&row_lookup_ab);

    let row_lookup_ab_local = adw::SwitchRow::builder()
        .title("Only use local address books for whitelist")
        .subtitle("Excludes external LDAP and GAL directories from whitelist")
        .active(false)
        .build();
    if let Some(s) = settings.clone() {
        row_lookup_ab_local.set_active(s.junk_lookup_addressbook_local_only());
    }
    row_lookup_ab_local.set_sensitive(row_lookup_ab.is_active());
    group.add(&row_lookup_ab_local);

    let row_empty_on_exit = adw::SwitchRow::builder()
        .title("Empty junk folder on application exit")
        .subtitle("Deletes messages in junk folder automatically on exit")
        .active(false)
        .build();
    if let Some(s) = settings.clone() {
        row_empty_on_exit.set_active(s.junk_empty_on_exit());
    }
    group.add(&row_empty_on_exit);

    let row_empty_days = adw::ComboRow::builder()
        .title("Retention period in junk folder")
        .subtitle(
            "Sets minimum age emails must have before being deleted on exit",
        )
        .model(&gtk::StringList::new(&[
            "Delete all messages on exit",
            "Only delete messages older than 1 day",
            "Only delete messages older than 7 days",
            "Only delete messages older than 14 days",
            "Only delete messages older than 30 days",
        ]))
        .build();
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
        .title("Default Spam Filter")
        .subtitle("Selects primary spam filter engine for local and remote filters")
        .model(&gtk::StringList::new(&["Bogofilter", "SpamAssassin"]))
        .build();

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
        row_plugin.set_subtitle("Neither 'bogofilter' nor 'spamassassin' was found in system PATH ($PATH). Local spam checking disabled.");
        let info_row = adw::ActionRow::builder()
            .title("No Spam Filter Program Installed")
            .subtitle("Neither 'bogofilter' nor 'spamassassin' is available in system PATH. Incoming emails can only be evaluated via server headers.")
            .build();
        info_row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        group.add(&info_row);
    } else {
        row_plugin.set_sensitive(row_check_incoming.is_active());
    }
    group.add(&row_plugin);

    if let Some(s) = settings.clone() {
        let row_plugin_ref = row_plugin.clone();
        let s_check = s.clone();
        row_check_incoming.connect_active_notify(move |row| {
            s_check.set_junk_check_incoming(row.is_active());
            row_plugin_ref.set_sensitive(row.is_active() && any_available);
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
        .title(&crate::i18n::gettext("New Mail Notification"))
        .description(&crate::i18n::gettext(
            "Notification banners and sound alerts for incoming messages (like GNOME Evolution)",
        ))
        .build();

    let row_notify = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Generate a notification for new messages"))
        .subtitle(&crate::i18n::gettext("Display a desktop banner notification when new emails arrive"))
        .build();

    let row_only_inbox = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Notify only for Inbox folders"))
        .subtitle(&crate::i18n::gettext("Ignore new messages arriving in other folders"))
        .build();

    let row_sound = adw::SwitchRow::builder()
        .title(&crate::i18n::gettext("Play a sound for new messages"))
        .subtitle(&crate::i18n::gettext("Audible alert when new messages arrive (throttled to once every 30 seconds)"))
        .build();

    let sound_types = gtk::StringList::new(&[
        &crate::i18n::gettext("Freedesktop sound theme (message-new-email)"),
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
        row_notify.set_active(s.mail_notification_enabled());
        row_only_inbox.set_active(s.mail_notification_only_inbox());
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
        row_notify.connect_active_notify(move |row| {
            s_notify.set_mail_notification_enabled(row.is_active());
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
        .subtitle(&crate::i18n::gettext(
            "Displays the unread counter in the window title and on the Mail tab",
        ))
        .build();
    if let Some(s) = settings.as_ref() {
        row_titlebar_counter.set_active(s.show_window_title_unread_counter());
        let s_counter = s.clone();
        row_titlebar_counter.connect_active_notify(move |row| {
            s_counter.set_show_window_title_unread_counter(row.is_active());
        });
    }

    group.add(&row_notify);
    group.add(&row_only_inbox);
    group.add(&row_titlebar_counter);
    group.add(&row_sound);
    group.add(&row_sound_type);
    group.add(&row_sound_file);

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
    fn local_account_rows_reorder_reconcile_defaults_and_keep_eds_read_only() {
        let source = include_str!("preferences_dialog.rs");
        assert!(source.contains("move_profile(&profile_id, -1)"));
        assert!(source.contains("move_profile(&profile_id, 1)"));
        assert!(source.contains("reconcile_local_default(&profile_id)"));
        assert!(source.contains("Default Account"));
        assert!(source.contains("read-only EDS discovery"));
        assert!(!source.contains(concat!(
            "AccountSetupDialog::new_for_edit(&dummy_",
            "profile)"
        )));
    }

    #[test]
    fn manual_filter_worker_boundary_captures_inputs_before_spawn() {
        let source = include_str!("preferences_dialog.rs");
        let manual_section_start = source
            .find("tooltip_text(\"Run rule now on local mail store\")")
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
    fn mail_reading_preferences_structure_and_citations_wired() {
        let full_source = include_str!("preferences_dialog.rs");
        let tests_pos = full_source
            .find("#[cfg(test)]")
            .or_else(|| full_source.find("mod tests"))
            .unwrap_or(full_source.len());
        let source = &full_source[..tests_pos];

        assert!(source.contains("mail_image_loading_policy"));
        assert!(source.contains("set_mail_image_loading_policy"));
        assert!(source.contains("mail_mark_seen"));
        assert!(source.contains("set_mail_mark_seen"));
        assert!(source.contains("mail_caret_mode"));
        assert!(source.contains("set_mail_caret_mode"));
        assert!(source.contains("mail_magic_spacebar"));
        assert!(source.contains("set_mail_magic_spacebar"));
        assert!(source.contains("mail_show_real_date"));
        assert!(source.contains("set_mail_show_real_date"));
        assert!(source.contains("mail_headers_collapsed"));
        assert!(source.contains("set_mail_headers_collapsed"));
        assert!(source.contains("mail_citation_highlighting"));
        assert!(source.contains("set_mail_citation_highlighting"));
        assert!(source.contains("Citations and Highlighting"));

        // Verify obsolete micromanagement controls have been cleaned out
        assert!(!source.contains("citation_color_configs"));
        assert!(!source.contains("group_mail_columns"));
        assert!(!source.contains("group_mail_fonts"));
        assert!(!source.contains("group_contacts_editor"));
        assert!(!source.contains("expander_custom_header"));
    }

    #[test]
    fn preferences_dialog_wires_all_five_previously_missing_settings() {
        let source = include_str!("preferences_dialog.rs");

        // 1. composer-charset in Composer General
        assert!(source.contains("composer_charset"));
        assert!(source.contains("set_composer_charset"));
        assert!(source.contains("row_composer_charset"));

        // 2. contact-completion-book-sources in Contacts Autocomplete
        assert!(source.contains("contact_completion_book_sources"));
        assert!(source.contains("set_contact_completion_book_sources"));
        assert!(source.contains("row_completion_book_sources"));

        // 3. mail-show-all-headers in Mail Headers
        assert!(source.contains("mail_show_all_headers"));
        assert!(source.contains("set_mail_show_all_headers"));
        assert!(source.contains("row_show_all_headers"));

        // 4. mail-show-deleted in Mail Reading
        assert!(source.contains("mail_show_deleted"));
        assert!(source.contains("set_mail_show_deleted"));
        assert!(source.contains("row_mail_show_deleted"));

        // 5. mail-show-mark-read-actions in Mail Reading
        assert!(source.contains("mail_show_mark_read_actions"));
        assert!(source.contains("set_mail_show_mark_read_actions"));
        assert!(source.contains("row_mail_show_mark_read_actions"));
    }

    #[test]
    fn confirm_destructive_actions_is_not_in_mail_reading() {
        let full_source = include_str!("preferences_dialog.rs");
        let tests_pos = full_source
            .find("#[cfg(test)]")
            .or_else(|| full_source.find("mod tests"))
            .unwrap_or(full_source.len());
        let source = &full_source[..tests_pos];
        assert!(!source.contains("group_mail_reading.add(&row_confirm_destructive)"));
        assert!(source.contains("page_appearance.add(&group_confirmations)"));
    }

    #[test]
    fn preferences_dialog_structural_reorganizations_are_wired() {
        let full_source = include_str!("preferences_dialog.rs");
        let tests_pos = full_source
            .find("#[cfg(test)]")
            .or_else(|| full_source.find("mod tests"))
            .unwrap_or(full_source.len());
        let source = &full_source[..tests_pos];

        // Filter group reconnected
        assert!(source.contains("page_accounts.add(&group_filters)"));


        // Local sources domain split
        assert!(source.contains("page_contacts.add(&group_contacts_local)"));
        assert!(source.contains("page_calendar_display.add(&group_calendar_local)"));
        assert!(!source.contains("group_local_sources"));

        // Mail layout controls rehomed to page_mail_layout
        assert!(source.contains("page_mail_layout.add(&group_mail_window_layout)"));
        assert!(source.contains("group_mail_window_layout.add(&row_mail_layout)"));
        assert!(source.contains("group_mail_window_layout.add(&row_mail_sidebar)"));
        assert!(source.contains("group_mail_window_layout.add(&row_mail_toolbar)"));
        assert!(source.contains("group_mail_window_layout.add(&row_mail_density)"));
    }

    #[test]
    fn test_detect_program_path_finds_standard_binary_and_rejects_missing() {
        use super::detect_program_path;
        assert!(detect_program_path("sh").is_some());
        assert!(detect_program_path("non_existent_binary_xyz_404_not_found").is_none());
    }

    #[test]
    fn test_mail_junk_group_subordinate_wiring_present_in_source() {
        let full_source = include_str!("preferences_dialog.rs");
        let tests_pos = full_source
            .find("#[cfg(test)]")
            .or_else(|| full_source.find("mod tests"))
            .unwrap_or(full_source.len());
        let source = &full_source[..tests_pos];

        assert!(source.contains("detect_program_path"));
        assert!(source.contains("row_lookup_ab_local.set_sensitive"));
        assert!(source.contains("row_empty_days.set_sensitive"));
        assert!(source.contains("row_plugin.set_sensitive"));
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
            980,
            "PreferencesDialog must have configured content width of 980"
        );
        assert_eq!(
            dialog.content_height(),
            720,
            "PreferencesDialog must have configured content height of 720"
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
        let row_reply = find_combo_by_title(widget, "Default Reply Style")
            .expect("Default Reply Style ComboRow must exist in PreferencesDialog");
        let row_forward = find_combo_by_title(widget, "Default Forward Style")
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
    fn test_shared_mailbox_preferences_and_action_rows() {
        let source = include_str!("preferences_dialog.rs");
        assert!(source.contains("\"Open Shared Mailbox…\""));
        assert!(source.contains("\"system-users-symbolic\""));
        assert!(source.contains("prompt_open_shared_mailbox"));
        assert!(source.contains("refresh_managed_account_rows"));
        assert!(source.contains("is_shared_mailbox"));
        assert!(source.contains("\"Shared Mailbox\""));
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
        let row_sync = find_combo_by_title(widget, "Check for new messages")
            .expect("Check for new messages ComboRow must exist");
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

        assert_eq!(SIDEBAR_SECTIONS.len(), 2, "Must have exactly 2 sidebar sections matching Hylki");
        assert_eq!(SIDEBAR_SECTIONS[0].section, "Accounts");
        assert_eq!(SIDEBAR_SECTIONS[1].section, "Settings");

        let total_pages: usize = SIDEBAR_SECTIONS.iter().map(|s| s.pages.len()).sum();
        assert_eq!(total_pages, 17, "Must define exactly 17 preferences pages");

        let expected_ids = [
            "accounts", "contacts", "labels", "certificates",
            "mail_general", "appearance", "mail_layout", "mail_reading", "composer", "replies", "signatures", "headers", "junk",
            "calendar", "calendar_display", "network", "backup",
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

        assert_eq!(split.min_sidebar_width(), 200.0);
        assert_eq!(split.max_sidebar_width(), 280.0);
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

        // Verify all 17 pages exist in detail_stack
        let expected_ids = [
            "accounts", "contacts",
            "mail_general", "mail_reading", "mail_layout", "composer", "replies", "signatures", "headers", "labels", "junk",
            "calendar", "calendar_display",
            "appearance", "certificates", "network", "backup",
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

        // Verify SwitchRow subtitles are indexed
        let notify_hit = search_index.iter().find(|h| h.haystack.contains("banner") || h.haystack.contains("desktop"));
        assert!(notify_hit.is_some(), "SwitchRow subtitle must be indexed into haystack");

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
        let sidebar_list = pages_scroll.child().and_then(|w| w.downcast::<gtk::ListBox>().ok()).unwrap();

        // Switch to "signatures"
        super::select_sidebar_page(&sidebar_list, "signatures");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("signatures"));

        // Switch to "appearance"
        super::select_sidebar_page(&sidebar_list, "appearance");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("appearance"));

        // Switch to "calendar_display"
        super::select_sidebar_page(&sidebar_list, "calendar_display");
        while context.iteration(false) {}
        assert_eq!(detail_stack.visible_child_name().as_deref(), Some("calendar_display"));

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
}






