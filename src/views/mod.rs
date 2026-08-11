// SPDX-License-Identifier: GPL-3.0-or-later
//
// Views module — contains all major page views of the application.

pub(crate) mod accessibility;
mod calendar_page;
mod contacts_page;
mod html_editor;
mod mail_page;
mod mail_viewer;
mod memos_page;
mod tasks_page;

pub use calendar_page::CalendarPage;
pub use contacts_page::ContactsPage;
pub use html_editor::{EditorContent, HtmlEditor};
pub use mail_page::MailPage;
pub use mail_viewer::MailViewer;
pub use memos_page::MemosPage;
pub use tasks_page::TasksPage;
