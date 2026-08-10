// SPDX-License-Identifier: GPL-3.0-or-later
//
// Dialogs module — modal windows and preferences.

pub mod about_dialog;
pub mod account_setup_dialog;
pub mod compose_dialog;
pub mod contact_dialog;
pub mod event_dialog;
pub mod filter_editor_dialog;
pub mod oof_dialog;
pub mod preferences_dialog;
pub mod shortcuts_dialog;

pub use about_dialog::show_about_dialog;
pub use account_setup_dialog::AccountSetupDialog;
pub use compose_dialog::ComposeDialog;
pub use contact_dialog::ContactDialog;
pub use event_dialog::EventDialog;
pub use filter_editor_dialog::FilterEditorDialog;
pub use oof_dialog::OutOfOfficeDialog;
pub use preferences_dialog::PreferencesDialog;
pub use shortcuts_dialog::show_shortcuts_dialog;
