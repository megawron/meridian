// SPDX-License-Identifier: GPL-3.0-or-later
//
// Views module — contains all major page views of the application.

pub(crate) mod accessibility;
mod calendar_page;
mod contacts_page;
#[cfg(target_os = "linux")]
mod html_editor;
#[cfg(not(target_os = "linux"))]
#[path = "html_editor_fallback.rs"]
mod html_editor;
mod mail_page;
mod mail_viewer;
mod memos_page;
mod tasks_page;

pub use calendar_page::CalendarPage;
pub use contacts_page::ContactsPage;
#[allow(unused_imports)]
pub use html_editor::EditorContent;
pub use html_editor::HtmlEditor;
pub use mail_page::MailPage;
pub use mail_viewer::MailViewer;
pub(crate) use mail_viewer::{MessageSearchResult, MessageSearchStatus};
pub use memos_page::MemosPage;
pub use tasks_page::TasksPage;
