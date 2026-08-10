// SPDX-License-Identifier: GPL-3.0-or-later
//
// Relm4 frontend components. Backend services deliberately stay outside this
// module so they remain framework-independent and testable.

mod calendar;
mod compose;
mod contacts;
mod mail;
mod mail_detail;
mod memos;
mod root;
mod tasks;

#[allow(unused_imports)]
pub use root::{FrontendInput, FrontendView, launch_preview};
