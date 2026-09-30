// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialogs module — modal windows and preferences.

use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::settings_service::AppSettings;

pub mod about_dialog;
pub mod account_setup_dialog;
pub mod advanced_search_dialog;
pub mod availability_dialog;
pub mod calendar_purge_dialog;
pub mod certificate_manager_dialog;
pub mod compose_dialog;
pub mod contact_dialog;
pub mod contact_search_dialog;
pub mod drafts_dialog;
pub mod event_dialog;
pub mod filter_editor_dialog;
pub mod folder_properties_dialog;
pub mod folder_sort_order_dialog;
pub mod folder_subscriptions_dialog;
pub mod global_search_dialog;
pub mod import_dialog;
pub mod manage_groups_dialog;
pub mod name_selector_dialog;
pub mod oof_dialog;
pub mod preferences_dialog;
pub mod quick_add_popover;
pub mod reminder_dialog;
pub mod search_folder_dialog;
pub mod shortcuts_dialog;
pub mod signature_editor_dialog;
pub mod source_properties_dialog;

pub use about_dialog::show_about_dialog;
pub use account_setup_dialog::AccountSetupDialog;
#[allow(unused_imports)]
pub use advanced_search_dialog::AdvancedSearchDialog;
pub use availability_dialog::AvailabilityDialog;
pub use calendar_purge_dialog::CalendarPurgeDialog;
pub use certificate_manager_dialog::CertificateManagerDialog;
pub use compose_dialog::ComposeDialog;
pub use contact_dialog::ContactDialog;
pub use contact_search_dialog::ContactSearchDialog;
pub use drafts_dialog::DraftsDialog;
pub use event_dialog::EventDialog;
pub use filter_editor_dialog::FilterEditorDialog;
pub use folder_properties_dialog::{FolderPropertiesDialog, FolderPropertiesSnapshot};
pub use folder_sort_order_dialog::{
    FolderSortOrderDialog, FolderSortOrderItem, FolderSortOrderOutcome,
};
pub use folder_subscriptions_dialog::FolderSubscriptionsDialog;
pub use global_search_dialog::GlobalSearchDialog;
pub use import_dialog::ImportDialog;
pub use manage_groups_dialog::ManageGroupsDialog;
pub use name_selector_dialog::NameSelectorDialog;
pub use oof_dialog::OutOfOfficeDialog;
pub use preferences_dialog::PreferencesDialog;
pub use reminder_dialog::{ReminderDialog, ReminderDialogAction};
pub use search_folder_dialog::SearchFolderDialog;
pub use shortcuts_dialog::show_shortcuts_dialog;
pub use signature_editor_dialog::SignatureEditorDialog;
#[allow(unused_imports)]
pub use source_properties_dialog::{SourcePropertiesDialog, SourcePropertiesUpdate};

/// Run a destructive operation immediately when confirmations are disabled;
/// otherwise present one consistent confirmation dialog for all data views.
///
/// The default is deliberately conservative: if GSettings is not available
/// during development, confirmation remains enabled.
pub fn run_destructive_action<W, F>(
    parent: &W,
    title: &str,
    description: &str,
    confirm_label: &str,
    action: F,
) where
    W: IsA<gtk::Widget>,
    F: FnOnce() + 'static,
{
    let confirmation_enabled = AppSettings::try_new()
        .map(|settings| settings.confirm_destructive_actions())
        .unwrap_or(true);
    if !confirmation_enabled {
        action();
        return;
    }

    let confirmation = adw::AlertDialog::new(Some(title), Some(description));
    let cancel_label = crate::i18n::gettext("Cancel");
    confirmation.add_responses(&[("cancel", &cancel_label), ("confirm", confirm_label)]);
    confirmation.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);

    let pending_action = Rc::new(RefCell::new(Some(action)));
    let pending_action_for_response = Rc::clone(&pending_action);
    confirmation.connect_response(None, move |dialog, response| {
        if response != "confirm" {
            return;
        }
        dialog.close();
        let action = pending_action_for_response.borrow_mut().take();
        if let Some(action) = action {
            action();
        }
    });
    confirmation.present(Some(parent));
}
