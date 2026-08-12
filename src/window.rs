// SPDX-License-Identifier: GPL-3.0-or-later
//
// Main application window — implements the three-column adaptive layout.
//
// Structure:
//   AdwApplicationWindow
//   └── AdwToastOverlay
//       └── AdwToolbarView
//           ├── [top] AdwHeaderBar
//           │   ├── [start] GtkToggleButton (sidebar show/hide)
//           │   ├── [end] AdwViewSwitcher (Mail/Calendar/Contacts)
//           │   ├── [end] GtkButton (Neue E-Mail)
//           │   └── [end] GtkMenuButton (primary menu)
//           └── [content] AdwViewStack
//               ├── "mail"     → MailPage (3-column split)
//               ├── "calendar" → CalendarPage
//               └── "contacts" → ContactsPage

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use crate::application::EvolutionApplication;
use crate::services::AppSettings;
use crate::views::{CalendarPage, ContactsPage, MailPage, MemosPage, TasksPage};

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Debug, Default)]
    pub struct EvolutionWindow {
        pub view_stack: RefCell<Option<adw::ViewStack>>,
        pub toast_overlay: adw::ToastOverlay,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EvolutionWindow {
        const NAME: &'static str = "EvolutionWindow";
        type Type = super::EvolutionWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for EvolutionWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let window = self.obj();

            window.set_title(Some("Evolution"));
            if let Some(settings) = AppSettings::try_new() {
                window.set_default_size(settings.window_width(), settings.window_height());
                if settings.is_maximized() {
                    window.maximize();
                }
            } else {
                window.set_default_size(1400, 900);
            }
            window.set_size_request(360, 480);

            window.setup_layout();
            window.setup_actions();

            tracing::debug!("Main window constructed");
        }
    }

    impl WidgetImpl for EvolutionWindow {
        fn realize(&self) {
            self.parent_realize();
        }
    }
    impl WindowImpl for EvolutionWindow {
        fn close_request(&self) -> glib::Propagation {
            let window = self.obj();
            if let Some(settings) = AppSettings::try_new() {
                settings.set_window_width(window.default_width());
                settings.set_window_height(window.default_height());
                settings.set_is_maximized(window.is_maximized());
                if let Some(view_stack) = self.view_stack.borrow().as_ref() {
                    if let Some(view_name) = view_stack.visible_child_name() {
                        settings.set_active_view(view_name.as_str());
                    }
                }
            }
            self.parent_close_request()
        }
    }
    impl ApplicationWindowImpl for EvolutionWindow {}
    impl AdwApplicationWindowImpl for EvolutionWindow {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct EvolutionWindow(ObjectSubclass<imp::EvolutionWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow,
                 gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap,
                    gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native,
                    gtk::Root, gtk::ShortcutManager;
}

impl EvolutionWindow {
    pub fn new(app: &EvolutionApplication) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// Add an AdwToast notification to the window.
    pub fn add_toast(&self, toast: &adw::Toast) {
        self.imp().toast_overlay.add_toast(toast.clone());
    }

    fn mail_page(&self) -> Option<MailPage> {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| {
                view_stack
                    .child_by_name("mail")
                    .and_then(|widget| widget.downcast::<MailPage>().ok())
            })
    }

    fn tasks_page(&self) -> Option<TasksPage> {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.child_by_name("tasks"))
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<TasksPage>().ok())
    }

    fn calendar_page(&self) -> Option<CalendarPage> {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.child_by_name("calendar"))
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<CalendarPage>().ok())
    }

    pub(crate) fn refresh_calendar_page(&self) {
        if let Some(calendar_page) = self.calendar_page() {
            calendar_page.reload_calendar_data();
        }
    }

    fn contacts_page(&self) -> Option<ContactsPage> {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.child_by_name("contacts"))
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<ContactsPage>().ok())
    }

    fn present_new_event(&self) {
        if let Some(calendar_page) = self.calendar_page() {
            calendar_page.present_new_event();
            return;
        }
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("calendar");
        }
        let window = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(window) = window.upgrade() {
                if let Some(calendar_page) = window.calendar_page() {
                    calendar_page.present_new_event();
                }
            }
        });
    }

    fn present_new_contact(&self) {
        if let Some(contacts_page) = self.contacts_page() {
            contacts_page.present_new_contact();
            return;
        }
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("contacts");
        }
        let window = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(window) = window.upgrade() {
                if let Some(contacts_page) = window.contacts_page() {
                    contacts_page.present_new_contact();
                }
            }
        });
    }

    fn present_account_setup(&self) {
        let dialog = crate::dialogs::AccountSetupDialog::new();
        dialog.set_transient_for(Some(self));
        let window = self.downgrade();
        dialog.connect_close_request(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_mail_page();
            }
            glib::Propagation::Proceed
        });
        dialog.present();
    }

    fn memos_page(&self) -> Option<MemosPage> {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.child_by_name("memos"))
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<MemosPage>().ok())
    }

    fn present_new_task(&self) {
        if let Some(tasks_page) = self.tasks_page() {
            tasks_page.present_new_task();
            return;
        }
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("tasks");
        }
        let window = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(window) = window.upgrade() {
                if let Some(tasks_page) = window.tasks_page() {
                    tasks_page.present_new_task();
                }
            }
        });
    }

    fn present_new_memo(&self) {
        if let Some(memos_page) = self.memos_page() {
            memos_page.present_new_memo();
            return;
        }
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("memos");
        }
        let window = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(window) = window.upgrade() {
                if let Some(memos_page) = window.memos_page() {
                    memos_page.present_new_memo();
                }
            }
        });
    }

    fn present_new_mail(&self) {
        let dialog = crate::dialogs::ComposeDialog::new();
        dialog.set_transient_for(Some(self));
        dialog.present();
    }

    fn present_contextual_new(&self) {
        let view_name = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .map(|name| name.to_string());
        match view_name.as_deref() {
            Some("calendar") => self.present_new_event(),
            Some("contacts") => self.present_new_contact(),
            Some("tasks") => self.present_new_task(),
            Some("memos") => self.present_new_memo(),
            _ => self.present_new_mail(),
        }
    }

    pub(crate) fn refresh_mail_page(&self) {
        if let Some(mail_page) = self.mail_page() {
            mail_page.refresh_mail();
        }
    }

    pub(crate) fn refresh_category_presentation(&self) {
        let Some(view_stack) = self.imp().view_stack.borrow().as_ref().cloned() else {
            return;
        };
        if let Some(calendar_page) = view_stack
            .child_by_name("calendar")
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<CalendarPage>().ok())
        {
            calendar_page.refresh_category_presentation();
        }
        if let Some(tasks_page) = view_stack
            .child_by_name("tasks")
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<crate::views::TasksPage>().ok())
        {
            tasks_page.refresh_category_presentation();
        }
        if let Some(memos_page) = view_stack
            .child_by_name("memos")
            .and_then(|widget| widget.downcast::<adw::Bin>().ok())
            .and_then(|bin| bin.child())
            .and_then(|widget| widget.downcast::<crate::views::MemosPage>().ok())
        {
            memos_page.refresh_category_presentation();
        }
    }

    pub(crate) fn show_search_result(
        &self,
        domain: crate::services::search_engine::SearchDomain,
        reference_id: String,
        query: String,
    ) {
        let page_name = match domain {
            crate::services::search_engine::SearchDomain::Mail => "mail",
            crate::services::search_engine::SearchDomain::Calendar => {
                let target = reference_id.split_once(':');
                let target = target.map(|(source_uid, uid)| (source_uid, uid));
                let snapshot = crate::services::calendar_store_service::CalendarStoreService::new()
                    .load_snapshot();
                if target.is_some_and(|(source_uid, uid)| {
                    snapshot
                        .tasks
                        .iter()
                        .any(|task| task.source_uid == source_uid && task.uid == uid)
                }) {
                    "tasks"
                } else if target.is_some_and(|(source_uid, uid)| {
                    snapshot
                        .memos
                        .iter()
                        .any(|memo| memo.source_uid == source_uid && memo.uid == uid)
                }) {
                    "memos"
                } else {
                    "calendar"
                }
            }
            crate::services::search_engine::SearchDomain::Contacts => "contacts",
            crate::services::search_engine::SearchDomain::All => return,
        };
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name(page_name);
        }
        let window = self.downgrade();
        let calendar_target = reference_id
            .split_once(':')
            .map(|(source_uid, uid)| (source_uid.to_string(), uid.to_string()));
        let mut attempts = 0u8;
        let mut query_applied = false;
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !query_applied {
                query_applied = match page_name {
                    "mail" => window.mail_page().is_some_and(|page| {
                        page.set_search_query(&query);
                        true
                    }),
                    "calendar" => window.calendar_page().is_some_and(|page| {
                        page.set_search_query(&query);
                        true
                    }),
                    "contacts" => window.contacts_page().is_some_and(|page| {
                        page.set_search_query(&query);
                        true
                    }),
                    "tasks" => window.tasks_page().is_some_and(|page| {
                        page.set_search_query(&query);
                        true
                    }),
                    "memos" => window.memos_page().is_some_and(|page| {
                        page.set_search_query(&query);
                        true
                    }),
                    _ => true,
                };
            }
            let found = match page_name {
                "mail" => window.mail_page().is_some_and(|page| {
                    reference_id.is_empty() || page.select_search_result(&reference_id)
                }),
                "calendar" => window.calendar_page().is_some_and(|page| {
                    calendar_target
                        .as_ref()
                        .is_none_or(|(source_uid, uid)| page.open_search_result(source_uid, uid))
                }),
                "contacts" => window.contacts_page().is_some_and(|page| {
                    reference_id.is_empty()
                        || page.select_search_result("local-addressbook", &reference_id)
                }),
                "tasks" => window.tasks_page().is_some_and(|page| {
                    calendar_target
                        .as_ref()
                        .is_none_or(|(source_uid, uid)| page.open_search_result(source_uid, uid))
                }),
                "memos" => window.memos_page().is_some_and(|page| {
                    calendar_target
                        .as_ref()
                        .is_none_or(|(source_uid, uid)| page.select_search_result(source_uid, uid))
                }),
                _ => true,
            };
            attempts = attempts.saturating_add(1);
            if found || attempts >= 30 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        self.add_toast(&adw::Toast::new(match page_name {
            "mail" => "Mailbereich geöffnet",
            "calendar" => "Kalenderbereich geöffnet und Treffer gesucht",
            "tasks" => "Aufgabenbereich geöffnet und Treffer gesucht",
            "memos" => "Notizbereich geöffnet und Treffer gesucht",
            _ => "Kontaktbereich geöffnet und Treffer gesucht",
        }));
    }

    fn focus_active_search(&self) {
        let view_name = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .map(|name| name.to_string());
        match view_name.as_deref() {
            Some("calendar") => self.calendar_page().map(|page| page.focus_search()),
            Some("contacts") => self.contacts_page().map(|page| page.focus_search()),
            Some("tasks") => self.tasks_page().map(|page| page.focus_search()),
            Some("memos") => self.memos_page().map(|page| page.focus_search()),
            _ => self.mail_page().map(|page| page.focus_search()),
        };
    }

    /// Add a checkable window action backed by a persistent GSettings boolean.
    ///
    /// Evolution's View menu exposes these options as live toggles.  Keeping
    /// the action state synchronized with GSettings means the check mark also
    /// follows changes made by preferences, responsive breakpoints, or the
    /// dedicated toolbar buttons.
    fn add_boolean_setting_action<Read, Write>(
        &self,
        name: &'static str,
        key: &'static str,
        read: Read,
        write: Write,
    ) -> gio::SimpleAction
    where
        Read: Fn(&AppSettings) -> bool + 'static,
        Write: Fn(&AppSettings, bool) + 'static,
    {
        let settings = AppSettings::try_new();
        let initial = settings.as_ref().map(&read).unwrap_or(false);
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let settings_for_activate = settings.clone();
        action.connect_activate(move |action, _| {
            let current = action
                .state()
                .and_then(|state| state.get::<bool>())
                .unwrap_or(false);
            let next = !current;
            action.set_state(&next.to_variant());
            if let Some(settings) = settings_for_activate.as_ref() {
                write(settings, next);
            }
        });

        if let Some(settings) = settings {
            let action_for_settings = action.clone();
            settings.connect_changed(Some(key), move |settings, _| {
                action_for_settings.set_state(&settings.boolean(key).to_variant());
            });
        }
        self.add_action(&action);
        action
    }

    /// Add a mutually exclusive-looking menu action for a persistent string
    /// setting.  Evolution presents mail density and layout as selected menu
    /// choices; boolean action state lets Gio render the current choice while
    /// keeping the existing action names and menu structure.
    fn add_string_setting_action<Read, Write>(
        &self,
        name: &'static str,
        key: &'static str,
        value: &'static str,
        read: Read,
        write: Write,
    ) -> gio::SimpleAction
    where
        Read: Fn(&AppSettings) -> String + 'static,
        Write: Fn(&AppSettings, &str) + 'static,
    {
        let settings = AppSettings::try_new();
        let initial = settings
            .as_ref()
            .is_some_and(|settings| read(settings) == value);
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let value_for_activate = value.to_string();
        let settings_for_activate = settings.clone();
        action.connect_activate(move |action, _| {
            if let Some(settings) = settings_for_activate.as_ref() {
                write(settings, &value_for_activate);
            }
            action.set_state(&true.to_variant());
        });

        if let Some(settings) = settings {
            let action_for_settings = action.clone();
            settings.connect_changed(Some(key), move |settings, _| {
                action_for_settings
                    .set_state(&(settings.string(key).as_str() == value).to_variant());
            });
        }
        self.add_action(&action);
        action
    }

    fn configure_primary_action(button: &gtk::Button, view_name: &str) {
        let (icon_name, tooltip, action_name) = match view_name {
            "calendar" => (
                "x-office-calendar-symbolic",
                "Neuen Termin erstellen (Ctrl+N)",
                "win.new",
            ),
            "contacts" => (
                "contact-new-symbolic",
                "Neuen Kontakt erstellen (Ctrl+N)",
                "win.new",
            ),
            "tasks" => (
                "task-new-symbolic",
                "Neue Aufgabe erstellen (Ctrl+N)",
                "win.new",
            ),
            "memos" => (
                "document-new-symbolic",
                "Neue Notiz erstellen (Ctrl+N)",
                "win.new",
            ),
            _ => (
                "mail-message-new-symbolic",
                "Neue E-Mail erstellen (Ctrl+N)",
                "win.new",
            ),
        };
        button.set_icon_name(icon_name);
        button.set_tooltip_text(Some(tooltip));
        button.set_action_name(Some(action_name));
        if view_name == "mail" {
            button.add_css_class("suggested-action");
        } else {
            button.remove_css_class("suggested-action");
        }
    }

    fn set_mail_actions_enabled(&self, enabled: bool) {
        const MAIL_ACTIONS: &[&str] = &[
            "toggle-sidebar",
            "toggle-preview",
            "toggle-mail-threading",
            "toggle-mail-preview-text",
            "toggle-mail-avatar",
            "toggle-mail-sender",
            "toggle-mail-recipient",
            "toggle-mail-date",
            "toggle-mail-attachment",
            "toggle-mail-labels",
            "mail-density-compact",
            "mail-density-normal",
            "mail-density-spacious",
            "use-classic-mail-layout",
            "use-modern-mail-layout",
            "toggle-mail-toolbar",
            "refresh-mail",
            "undo",
            "redo",
            "reply",
            "reply-all",
            "forward",
            "export",
            "delete",
            "mark-unread",
            "mark-read",
            "select-all-mail",
            "toggle-flag",
            "toggle-important",
            "archive",
            "mark-junk",
            "mark-not-junk",
            "new-folder",
            "empty-trash",
            "expunge",
            "search",
            "mail-next",
            "mail-prev",
        ];
        for name in MAIL_ACTIONS {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
        self.set_message_actions_enabled(
            enabled
                && self
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.has_selected_message()),
        );
    }

    fn set_message_actions_enabled(&self, enabled: bool) {
        const MESSAGE_ACTIONS: &[&str] = &[
            "reply",
            "reply-all",
            "forward",
            "export",
            "delete",
            "mark-unread",
            "mark-read",
            "toggle-flag",
            "toggle-important",
            "archive",
            "mark-junk",
            "mark-not-junk",
        ];
        for name in MESSAGE_ACTIONS {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
    }

    fn setup_layout(&self) {
        let setup_started = std::time::Instant::now();
        tracing::debug!("Main window layout setup started");
        let view_stack = adw::ViewStack::new();

        let mail_page = MailPage::new();
        view_stack.add_titled_with_icon(&mail_page, Some("mail"), "Mail", "mail-unread-symbolic");
        tracing::debug!(
            elapsed_ms = setup_started.elapsed().as_millis() as u64,
            "Mail page added to view stack"
        );

        // Construct only the visible mail module at startup.  EDS/ECal/EBook
        // page construction starts native worker activity; eagerly creating
        // every hidden module can overwhelm a cold Linux EDS session before
        // the first frame has settled.
        let calendar_bin = Self::lazy_module_bin("Kalender");
        view_stack.add_titled_with_icon(
            &calendar_bin,
            Some("calendar"),
            "Kalender",
            "x-office-calendar-symbolic",
        );

        let contacts_bin = Self::lazy_module_bin("Kontakte");
        view_stack.add_titled_with_icon(
            &contacts_bin,
            Some("contacts"),
            "Kontakte",
            "contact-new-symbolic",
        );

        let tasks_bin = Self::lazy_module_bin("Aufgaben");
        view_stack.add_titled_with_icon(
            &tasks_bin,
            Some("tasks"),
            "Aufgaben",
            "tasks-due-symbolic",
        );

        let memos_bin = Self::lazy_module_bin("Notizen");
        view_stack.add_titled_with_icon(&memos_bin, Some("memos"), "Notizen", "note-symbolic");

        let ensure_module_loaded: Rc<dyn Fn(&str)> = {
            let calendar_bin = calendar_bin.clone();
            let contacts_bin = contacts_bin.clone();
            let tasks_bin = tasks_bin.clone();
            let memos_bin = memos_bin.clone();
            Rc::new(move |name| match name {
                "calendar"
                    if calendar_bin
                        .child()
                        .map(|child| child.is::<adw::StatusPage>())
                        .unwrap_or(true) =>
                {
                    calendar_bin.set_child(Some(&CalendarPage::new()));
                }
                "contacts"
                    if contacts_bin
                        .child()
                        .map(|child| child.is::<adw::StatusPage>())
                        .unwrap_or(true) =>
                {
                    contacts_bin.set_child(Some(&ContactsPage::new()));
                }
                "tasks"
                    if tasks_bin
                        .child()
                        .map(|child| child.is::<adw::StatusPage>())
                        .unwrap_or(true) =>
                {
                    tasks_bin.set_child(Some(&crate::views::TasksPage::new()));
                }
                "memos"
                    if memos_bin
                        .child()
                        .map(|child| child.is::<adw::StatusPage>())
                        .unwrap_or(true) =>
                {
                    memos_bin.set_child(Some(&crate::views::MemosPage::new()));
                }
                _ => {}
            })
        };

        let active_view = AppSettings::try_new()
            .map(|settings| settings.active_view())
            .filter(|view| {
                matches!(
                    view.as_str(),
                    "mail" | "calendar" | "contacts" | "tasks" | "memos"
                )
            })
            .unwrap_or_else(|| "mail".to_string());
        view_stack.set_visible_child_name(&active_view);
        // The visible-child notification is connected below, so it does not
        // fire for the initial persisted view.  Load that module explicitly
        // to make a restored Calendar/Contacts/Tasks/Memos session usable
        // immediately instead of leaving its placeholder visible.
        ensure_module_loaded(&active_view);

        if let Some(settings) = AppSettings::try_new() {
            let ensure_module_loaded_for_signal = Rc::clone(&ensure_module_loaded);
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(view_name) = stack.visible_child_name() {
                    ensure_module_loaded_for_signal(view_name.as_str());
                    settings.set_active_view(view_name.as_str());
                }
            });
        }

        let header_bar = adw::HeaderBar::new();

        let sidebar_visible = AppSettings::try_new()
            .map(|settings| settings.mail_show_sidebar())
            .unwrap_or(true);
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Sidebar ein-/ausblenden (F9)")
            .active(sidebar_visible)
            .build();
        sidebar_button.set_visible(active_view == "mail");

        sidebar_button.connect_clicked(glib::clone!(
            #[weak]
            mail_page,
            move |btn| {
                mail_page.set_sidebar_visible(btn.is_active());
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_show_sidebar(btn.is_active());
                }
            }
        ));

        if let Some(settings) = AppSettings::try_new() {
            let sidebar_button_for_settings = sidebar_button.clone();
            settings.connect_changed(Some("mail-show-sidebar"), move |settings, _| {
                sidebar_button_for_settings.set_active(settings.boolean("mail-show-sidebar"));
            });
        }

        header_bar.pack_start(&sidebar_button);

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();

        header_bar.set_title_widget(Some(&view_switcher));

        let menu_model = Self::build_primary_menu();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu_model)
            .tooltip_text("Hauptmenü")
            .primary(true)
            .build();

        header_bar.pack_end(&menu_button);

        let compose_button = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text("Neue E-Mail (Ctrl+N)")
            .action_name("app.compose")
            .css_classes(vec!["suggested-action"])
            .build();

        Self::configure_primary_action(&compose_button, &active_view);
        let primary_action_for_view = compose_button.clone();
        let sidebar_button_for_view = sidebar_button.clone();
        view_stack.connect_visible_child_name_notify(move |stack| {
            if let Some(view_name) = stack.visible_child_name() {
                Self::configure_primary_action(&primary_action_for_view, view_name.as_str());
                sidebar_button_for_view.set_visible(view_name.as_str() == "mail");
            }
        });

        header_bar.pack_end(&compose_button);

        let statusbar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(vec!["statusbar"])
            .build();

        let online_toggle = gtk::Button::builder()
            .icon_name("user-available-symbolic")
            .tooltip_text("Online-Status (Online/Offline umschalten)")
            .css_classes(vec!["flat", "circular"])
            .build();

        let status_label = gtk::Label::builder()
            .label("Evolution GTK4  ·  Bereit")
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();

        let status_progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .pulse_step(0.1)
            .build();

        if let Some(settings) = AppSettings::try_new() {
            online_toggle.set_icon_name(if settings.offline_mode() {
                "network-wireless-offline-symbolic"
            } else {
                "user-available-symbolic"
            });
            online_toggle.set_tooltip_text(Some(if settings.offline_mode() {
                "Offline-Modus aktiv; auf Online umschalten"
            } else {
                "Online-Modus aktiv; auf Offline umschalten"
            }));
            let settings_for_online = settings.clone();
            let status_label_for_online = status_label.clone();
            let online_toggle_for_settings = online_toggle.clone();
            online_toggle.connect_clicked(move |_| {
                let offline = !settings_for_online.offline_mode();
                settings_for_online.set_offline_mode(offline);
                online_toggle_for_settings.set_icon_name(if offline {
                    "network-wireless-offline-symbolic"
                } else {
                    "user-available-symbolic"
                });
                online_toggle_for_settings.set_tooltip_text(Some(if offline {
                    "Offline-Modus aktiv; auf Online umschalten"
                } else {
                    "Online-Modus aktiv; auf Offline umschalten"
                }));
                status_label_for_online.set_label(if offline {
                    "Evolution GTK4  ·  Offline"
                } else {
                    "Evolution GTK4  ·  Online"
                });
            });
            let online_toggle_for_signal = online_toggle.clone();
            let status_label_for_signal = status_label.clone();
            settings.connect_changed(Some("offline-mode"), move |settings, _| {
                let offline = settings.boolean("offline-mode");
                online_toggle_for_signal.set_icon_name(if offline {
                    "network-wireless-offline-symbolic"
                } else {
                    "user-available-symbolic"
                });
                status_label_for_signal.set_label(if offline {
                    "Evolution GTK4  ·  Offline"
                } else {
                    "Evolution GTK4  ·  Online"
                });
            });
            status_label.set_label(if settings.offline_mode() {
                "Evolution GTK4  ·  Offline"
            } else {
                "Evolution GTK4  ·  Online"
            });
        } else {
            online_toggle.set_sensitive(false);
        }

        statusbar_box.append(&online_toggle);
        statusbar_box.append(&status_label);
        statusbar_box.append(&status_progress);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.add_bottom_bar(&statusbar_box);
        toolbar_view.set_content(Some(&view_stack));

        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.set_hexpand(true);
        toast_overlay.set_vexpand(true);
        toast_overlay.set_child(Some(&toolbar_view));

        // Responsive Breakpoints
        let bp_narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        // FIXME: Hiding the preview widget causes layout oscillation if its minimum width
        // forces the window size to cross the breakpoint threshold continuously.
        // if let Some(preview) = mail_page.preview_widget() {
        //     bp_narrow.add_setter(&preview, "visible", Some(&false.to_value()));
        // }

        let bp_mobile =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 560sp").unwrap());
        bp_mobile.add_setter(mail_page.outer_paned(), "position", Some(&0_i32.to_value()));
        // if let Some(preview) = mail_page.preview_widget() {
        //     bp_mobile.add_setter(&preview, "visible", Some(&false.to_value()));
        // }
        bp_mobile.add_setter(&sidebar_button, "active", Some(&false.to_value()));

        self.add_breakpoint(bp_narrow);
        self.add_breakpoint(bp_mobile);

        self.imp().view_stack.replace(Some(view_stack));
        self.set_content(Some(toast_overlay));
        tracing::debug!(
            elapsed_ms = setup_started.elapsed().as_millis() as u64,
            "Main window layout installed"
        );

        // Let the first frame become interactive before restoring a non-mail
        // module from settings.  Mail remains the fast default; a previously
        // selected calendar/contacts/tasks/memos view is initialized on the
        // next main-loop turn instead of inside window construction.
        if active_view != "mail" {
            let ensure_module_loaded_for_initial = Rc::clone(&ensure_module_loaded);
            glib::idle_add_local_once(move || {
                ensure_module_loaded_for_initial(&active_view);
            });
        }
    }

    fn lazy_module_bin(title: &str) -> adw::Bin {
        let status = adw::StatusPage::builder()
            .title(title)
            .description("Dieser Bereich wird beim ersten Öffnen geladen.")
            .icon_name("view-refresh-symbolic")
            .vexpand(true)
            .hexpand(true)
            .build();
        adw::Bin::builder().child(&status).build()
    }

    fn build_primary_menu() -> gio::Menu {
        let menu = gio::Menu::new();

        // ─── 1. Datei (File) ───────────────────────────────────────────────
        let file_menu = gio::Menu::new();
        let new_menu = gio::Menu::new();
        new_menu.append(Some("Neue E-Mail"), Some("app.compose"));
        new_menu.append(Some("Neuer Kontakt"), Some("win.new-contact"));
        new_menu.append(Some("Neuer Termin"), Some("win.new-event"));
        new_menu.append(Some("Neue Aufgabe"), Some("win.new-task"));
        new_menu.append(Some("Neue Notiz"), Some("win.new-memo"));
        new_menu.append(Some("Neuer Ordner..."), Some("win.new-folder"));
        file_menu.append_submenu(Some("Neu"), &new_menu);
        file_menu.append(Some("Lokale Entwürfe"), Some("app.drafts"));
        file_menu.append(Some("Ausgewählte E-Mail exportieren"), Some("win.export"));
        menu.append_submenu(Some("Datei"), &file_menu);

        // ─── 2. Bearbeiten (Edit) ──────────────────────────────────────────
        let edit_menu = gio::Menu::new();
        edit_menu.append(Some("Rückgängig"), Some("win.undo"));
        edit_menu.append(Some("Wiederholen"), Some("win.redo"));
        edit_menu.append(Some("E-Mail suchen"), Some("win.search"));
        edit_menu.append(Some("Globale Suche"), Some("app.global-search"));
        edit_menu.append(
            Some("Filterregeln bearbeiten..."),
            Some("win.filter-editor"),
        );
        edit_menu.append(
            Some("Suchordner verwalten..."),
            Some("win.search-folder-editor"),
        );
        edit_menu.append(Some("Einstellungen"), Some("app.preferences"));
        menu.append_submenu(Some("Bearbeiten"), &edit_menu);

        // ─── 3. Ansicht (View) ─────────────────────────────────────────────
        let view_menu = gio::Menu::new();
        let switch_menu = gio::Menu::new();
        switch_menu.append(Some("Mail"), Some("win.switch-mail"));
        switch_menu.append(Some("Kalender"), Some("win.switch-calendar"));
        switch_menu.append(Some("Kontakte"), Some("win.switch-contacts"));
        switch_menu.append(Some("Aufgaben"), Some("win.switch-tasks"));
        switch_menu.append(Some("Notizen"), Some("win.switch-memos"));
        view_menu.append_submenu(Some("Bereich wechseln"), &switch_menu);

        view_menu.append(
            Some("Ordner-Seitenleiste ein-/ausblenden"),
            Some("win.toggle-sidebar"),
        );
        view_menu.append(
            Some("Vorschaufenster ein-/ausblenden"),
            Some("win.toggle-preview"),
        );
        view_menu.append(
            Some("Klassische Evolution-Mail-Anordnung"),
            Some("win.use-classic-mail-layout"),
        );
        view_menu.append(
            Some("Moderne Mail-Anordnung"),
            Some("win.use-modern-mail-layout"),
        );
        view_menu.append(
            Some("Klassische Mail-Werkzeugleiste ein-/ausblenden"),
            Some("win.toggle-mail-toolbar"),
        );
        view_menu.append(
            Some("Unterhaltungen gruppieren/auflösen"),
            Some("win.toggle-mail-threading"),
        );
        view_menu.append(
            Some("Vorschautext in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-preview-text"),
        );
        view_menu.append(
            Some("Avatare in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-avatar"),
        );
        view_menu.append(
            Some("Absender in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-sender"),
        );
        view_menu.append(
            Some("Empfänger in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-recipient"),
        );
        view_menu.append(
            Some("Datum in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-date"),
        );
        view_menu.append(
            Some("Anhangssymbole in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-attachment"),
        );
        view_menu.append(
            Some("Labels in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-labels"),
        );
        let density_menu = gio::Menu::new();
        density_menu.append(Some("Kompakt"), Some("win.mail-density-compact"));
        density_menu.append(Some("Standard"), Some("win.mail-density-normal"));
        density_menu.append(Some("Geräumig"), Some("win.mail-density-spacious"));
        view_menu.append_submenu(Some("Nachrichtendichte"), &density_menu);
        menu.append_submenu(Some("Ansicht"), &view_menu);

        // ─── 4. Nachricht (Message) ────────────────────────────────────────
        let message_menu = gio::Menu::new();
        message_menu.append(Some("Antworten"), Some("win.reply"));
        message_menu.append(Some("Allen antworten"), Some("win.reply-all"));
        message_menu.append(Some("Weiterleiten"), Some("win.forward"));
        message_menu.append(
            Some("Alle sichtbaren Nachrichten auswählen"),
            Some("win.select-all-mail"),
        );
        message_menu.append(Some("Als gelesen markieren"), Some("win.mark-read"));
        message_menu.append(Some("Als ungelesen markieren"), Some("win.mark-unread"));
        message_menu.append(Some("Markierung umschalten"), Some("win.toggle-flag"));
        message_menu.append(
            Some("Wichtigkeitsmarkierung umschalten"),
            Some("win.toggle-important"),
        );
        message_menu.append(Some("Archivieren"), Some("win.archive"));
        message_menu.append(Some("Als Spam markieren"), Some("win.mark-junk"));
        message_menu.append(Some("Als Nicht-Spam markieren"), Some("win.mark-not-junk"));
        message_menu.append(Some("Löschen"), Some("win.delete"));
        menu.append_submenu(Some("Nachricht"), &message_menu);

        // ─── 5. Ordner (Folder) ────────────────────────────────────────────
        let folder_menu = gio::Menu::new();
        folder_menu.append(Some("Neuer Ordner..."), Some("win.new-folder"));
        folder_menu.append(Some("Mülleimer leeren"), Some("win.empty-trash"));
        folder_menu.append(Some("Endgültig löschen (Expunge)"), Some("win.expunge"));
        menu.append_submenu(Some("Ordner"), &folder_menu);

        // ─── 6. Extras (Tools) ─────────────────────────────────────────────
        let tools_menu = gio::Menu::new();
        tools_menu.append(Some("Senden & Empfangen"), Some("win.refresh-mail"));
        tools_menu.append(Some("Abwesenheitsnotiz (OOF)..."), Some("win.oof-settings"));
        tools_menu.append(
            Some("Filterregeln bearbeiten..."),
            Some("win.filter-editor"),
        );
        tools_menu.append(
            Some("Suchordner verwalten..."),
            Some("win.search-folder-editor"),
        );
        tools_menu.append(Some("Kontoeinstellungen..."), Some("win.account-setup"));
        menu.append_submenu(Some("Extras"), &tools_menu);

        // ─── 7. Hilfe (Help) ───────────────────────────────────────────────
        let help_menu = gio::Menu::new();
        help_menu.append(Some("Tastenkombinationen"), Some("win.show-help-overlay"));
        help_menu.append(Some("Über Evolution"), Some("app.about"));
        menu.append_submenu(Some("Hilfe"), &help_menu);

        menu
    }

    fn setup_actions(&self) {
        let action_new = gio::SimpleAction::new("new", None);
        action_new.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_contextual_new()
        ));
        self.add_action(&action_new);

        let action_sidebar = self.add_boolean_setting_action(
            "toggle-sidebar",
            "mail-show-sidebar",
            AppSettings::mail_show_sidebar,
            AppSettings::set_mail_show_sidebar,
        );
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            let action_sidebar_for_view = action_sidebar.clone();
            view_stack.connect_visible_child_name_notify(move |stack| {
                action_sidebar_for_view.set_enabled(
                    stack
                        .visible_child_name()
                        .is_some_and(|view_name| view_name == "mail"),
                );
            });
            action_sidebar.set_enabled(
                view_stack
                    .visible_child_name()
                    .is_some_and(|view_name| view_name == "mail"),
            );
        }
        self.add_boolean_setting_action(
            "toggle-preview",
            "show-message-preview",
            AppSettings::show_message_preview,
            AppSettings::set_show_message_preview,
        );
        self.add_boolean_setting_action(
            "toggle-mail-threading",
            "mail-threading",
            AppSettings::mail_threading,
            AppSettings::set_mail_threading,
        );
        self.add_boolean_setting_action(
            "toggle-mail-preview-text",
            "mail-show-preview-text",
            AppSettings::mail_show_preview_text,
            AppSettings::set_mail_show_preview_text,
        );
        self.add_boolean_setting_action(
            "toggle-mail-avatar",
            "mail-show-avatar",
            AppSettings::mail_show_avatar,
            AppSettings::set_mail_show_avatar,
        );
        self.add_boolean_setting_action(
            "toggle-mail-sender",
            "mail-show-sender",
            AppSettings::mail_show_sender,
            AppSettings::set_mail_show_sender,
        );
        self.add_boolean_setting_action(
            "toggle-mail-recipient",
            "mail-show-recipient",
            AppSettings::mail_show_recipient,
            AppSettings::set_mail_show_recipient,
        );
        self.add_boolean_setting_action(
            "toggle-mail-date",
            "mail-show-date",
            AppSettings::mail_show_date,
            AppSettings::set_mail_show_date,
        );
        self.add_boolean_setting_action(
            "toggle-mail-attachment",
            "mail-show-attachment",
            AppSettings::mail_show_attachment,
            AppSettings::set_mail_show_attachment,
        );
        self.add_boolean_setting_action(
            "toggle-mail-labels",
            "mail-show-labels",
            AppSettings::mail_show_labels,
            AppSettings::set_mail_show_labels,
        );

        for (action_name, density) in [
            ("mail-density-compact", "compact"),
            ("mail-density-normal", "comfortable"),
            ("mail-density-spacious", "spacious"),
        ] {
            self.add_string_setting_action(
                action_name,
                "mail-row-density",
                density,
                AppSettings::mail_row_density,
                AppSettings::set_mail_row_density,
            );
        }

        for (action_name, layout) in [
            ("use-classic-mail-layout", "classic"),
            ("use-modern-mail-layout", "modern"),
        ] {
            self.add_string_setting_action(
                action_name,
                "mail-layout",
                layout,
                AppSettings::mail_layout,
                AppSettings::set_mail_layout,
            );
        }

        self.add_boolean_setting_action(
            "toggle-mail-toolbar",
            "mail-show-toolbar",
            AppSettings::mail_show_toolbar,
            AppSettings::set_mail_show_toolbar,
        );

        let action_refresh_mail = gio::SimpleAction::new("refresh-mail", None);
        action_refresh_mail.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                if let Some(mail_page) = window
                    .imp()
                    .view_stack
                    .borrow()
                    .as_ref()
                    .and_then(|stack| stack.child_by_name("mail"))
                    .and_then(|widget| widget.downcast::<MailPage>().ok())
                {
                    mail_page.refresh_mail();
                }
            }
        ));
        self.add_action(&action_refresh_mail);

        let action_undo = gio::SimpleAction::new("undo", None);
        action_undo.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .imp()
                    .view_stack
                    .borrow()
                    .as_ref()
                    .and_then(|view_stack| {
                        view_stack
                            .child_by_name("mail")
                            .and_then(|widget| widget.downcast::<MailPage>().ok())
                    })
                    .map(|mail_page| mail_page.undo());

                let message = match result {
                    Some(Ok(true)) => "Letzte Mailaktion rückgängig gemacht".to_string(),
                    Some(Ok(false)) => "Keine Mailaktion zum Rückgängigmachen".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail undo operation was not applied");
                        format!("Rückgängig nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_undo);

        let action_redo = gio::SimpleAction::new("redo", None);
        action_redo.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .imp()
                    .view_stack
                    .borrow()
                    .as_ref()
                    .and_then(|view_stack| {
                        view_stack
                            .child_by_name("mail")
                            .and_then(|widget| widget.downcast::<MailPage>().ok())
                    })
                    .map(|mail_page| mail_page.redo());

                let message = match result {
                    Some(Ok(true)) => "Letzte Mailaktion wiederholt".to_string(),
                    Some(Ok(false)) => "Keine Mailaktion zum Wiederholen".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail redo operation was not applied");
                        format!("Wiederholen nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_redo);

        for (action_name, stack_page) in [
            ("switch-mail", "mail"),
            ("switch-calendar", "calendar"),
            ("switch-contacts", "contacts"),
            ("switch-tasks", "tasks"),
            ("switch-memos", "memos"),
        ] {
            let action = gio::SimpleAction::new(action_name, None);
            let page_name = stack_page.to_string();
            action.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| {
                    if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
                        view_stack.set_visible_child_name(&page_name);
                    }
                }
            ));
            self.add_action(&action);
        }

        // Mail Action Handlers (Reply, Reply All, Forward, Delete)
        let action_reply = gio::SimpleAction::new("reply", None);
        action_reply.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.reply_selected() {
                        tracing::debug!("Reply action ignored without a selected message");
                    }
                }
            }
        ));
        self.add_action(&action_reply);

        let action_reply_all = gio::SimpleAction::new("reply-all", None);
        action_reply_all.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.reply_all_selected() {
                        tracing::debug!("Reply-all action ignored without a selected message");
                    }
                }
            }
        ));
        self.add_action(&action_reply_all);

        let action_forward = gio::SimpleAction::new("forward", None);
        action_forward.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.forward_selected() {
                        tracing::debug!("Forward action ignored without a selected message");
                    }
                }
            }
        ));
        self.add_action(&action_forward);

        let action_export = gio::SimpleAction::new("export", None);
        action_export.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let exported = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.export_selected());
                if !exported {
                    win.add_toast(&adw::Toast::new("Keine E-Mail zum Exportieren ausgewählt"));
                }
            }
        ));
        self.add_action(&action_export);

        let action_delete = gio::SimpleAction::new("delete", None);
        action_delete.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let Some(mail_page) = win.mail_page() else {
                    win.add_toast(&adw::Toast::new("Mailansicht ist nicht verfügbar"));
                    return;
                };
                if !mail_page.has_selected_message() {
                    win.add_toast(&adw::Toast::new("Keine Nachricht ausgewählt"));
                    return;
                }
                let win_for_action = win.clone();
                crate::dialogs::run_destructive_action(
                    &win,
                    "Nachricht löschen?",
                    "Die ausgewählte Nachricht wird dauerhaft gelöscht bzw. in den Papierkorb verschoben.",
                    "Löschen",
                    move || {
                        let result = win_for_action.mail_page().map(|mail_page| {
                            mail_page.delete_selected()
                        });
                        let message = match result {
                            Some(Ok(true)) => "Nachricht gelöscht".to_string(),
                            Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                            Some(Err(error)) => {
                                tracing::warn!(%error, "Mail delete operation was not applied");
                                format!("Löschen nicht möglich: {error}")
                            }
                            None => "Mailansicht ist nicht verfügbar".to_string(),
                        };
                        win_for_action.add_toast(&adw::Toast::new(&message));
                    },
                );
            }
        ));
        self.add_action(&action_delete);

        let action_mark_unread = gio::SimpleAction::new("mark-unread", None);
        action_mark_unread.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .imp()
                    .view_stack
                    .borrow()
                    .as_ref()
                    .and_then(|view_stack| {
                        view_stack
                            .child_by_name("mail")
                            .and_then(|widget| widget.downcast::<MailPage>().ok())
                    })
                    .map(|mail_page| mail_page.mark_selected_unread());

                let message = match result {
                    Some(Ok(true)) => "Nachricht als ungelesen markiert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mark-unread operation was not applied");
                        format!("Markieren nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_mark_unread);

        let action_mark_read = gio::SimpleAction::new("mark-read", None);
        action_mark_read.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.mark_selected_read());
                let message = match result {
                    Some(Ok(true)) => "Nachricht als gelesen markiert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mark-read operation was not applied");
                        format!("Als gelesen markieren nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_mark_read);

        let action_select_all_mail = gio::SimpleAction::new("select-all-mail", None);
        action_select_all_mail.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let count = win
                    .mail_page()
                    .map(|mail_page| mail_page.select_all_visible_messages())
                    .unwrap_or(0);
                win.add_toast(&adw::Toast::new(&format!(
                    "{count} sichtbare Nachricht(en) ausgewählt"
                )));
            }
        ));
        self.add_action(&action_select_all_mail);

        let action_toggle_flag = gio::SimpleAction::new("toggle-flag", None);
        action_toggle_flag.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.toggle_selected_flagged());
                let message = match result {
                    Some(Ok(true)) => "Markierung geändert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail flag operation was not applied");
                        format!("Markierung nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_toggle_flag);

        let action_toggle_important = gio::SimpleAction::new("toggle-important", None);
        action_toggle_important.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.toggle_selected_important());
                let message = match result {
                    Some(Ok(true)) => "Wichtigkeitsmarkierung geändert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail important operation was not applied");
                        format!("Wichtigkeitsmarkierung nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_toggle_important);

        let action_archive = gio::SimpleAction::new("archive", None);
        action_archive.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.archive_selected());
                let message = match result {
                    Some(Ok(true)) => "Nachricht archiviert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail archive operation was not applied");
                        format!("Archivieren nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_archive);

        let action_mark_junk = gio::SimpleAction::new("mark-junk", None);
        action_mark_junk.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.mark_selected_junk());
                let message = match result {
                    Some(Ok(true)) => "Nachricht als Spam markiert".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail junk operation was not applied");
                        format!("Spam-Markierung nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_mark_junk);

        let action_mark_not_junk = gio::SimpleAction::new("mark-not-junk", None);
        action_mark_not_junk.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.mark_selected_not_junk());
                let message = match result {
                    Some(Ok(true)) => "Spam-Markierung entfernt".to_string(),
                    Some(Ok(false)) => "Keine Nachricht ausgewählt".to_string(),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail not-junk operation was not applied");
                        format!("Nicht-Spam-Markierung nicht möglich: {error}")
                    }
                    None => "Mailansicht ist nicht verfügbar".to_string(),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_mark_not_junk);

        let action_shortcuts = gio::SimpleAction::new("show-help-overlay", None);
        action_shortcuts.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                crate::dialogs::show_shortcuts_dialog(&win);
            }
        ));
        self.add_action(&action_shortcuts);

        let action_oof = gio::SimpleAction::new("oof-settings", None);
        action_oof.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::OutOfOfficeDialog::new();
                dialog.set_transient_for(Some(&win));
                dialog.present();
            }
        ));
        self.add_action(&action_oof);

        let action_filter = gio::SimpleAction::new("filter-editor", None);
        action_filter.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::FilterEditorDialog::new();
                dialog.set_transient_for(Some(&win));
                dialog.present();
            }
        ));
        self.add_action(&action_filter);

        let action_search_folder = gio::SimpleAction::new("search-folder-editor", None);
        action_search_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::SearchFolderDialog::new("");
                dialog.set_transient_for(Some(&win));
                dialog.present();
            }
        ));
        self.add_action(&action_search_folder);

        let action_account_setup = gio::SimpleAction::new("account-setup", None);
        action_account_setup.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_account_setup()
        ));
        self.add_action(&action_account_setup);

        let action_new_contact = gio::SimpleAction::new("new-contact", None);
        action_new_contact.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_new_contact()
        ));
        self.add_action(&action_new_contact);

        let action_new_event = gio::SimpleAction::new("new-event", None);
        action_new_event.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_new_event()
        ));
        self.add_action(&action_new_event);

        let action_new_task = gio::SimpleAction::new("new-task", None);
        action_new_task.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_new_task()
        ));
        self.add_action(&action_new_task);

        let action_new_memo = gio::SimpleAction::new("new-memo", None);
        action_new_memo.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_new_memo()
        ));
        self.add_action(&action_new_memo);

        let action_new_folder = gio::SimpleAction::new("new-folder", None);
        action_new_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_new_local_folder_dialog();
                } else {
                    win.add_toast(&adw::Toast::new("Mailansicht ist nicht verfügbar"));
                }
            }
        ));
        self.add_action(&action_new_folder);

        for (action_name, heading, body, success) in [
            (
                "empty-trash",
                "Papierkorb leeren?",
                "Lokale app-eigene Nachrichten werden aus der sichtbaren Papierkorbansicht entfernt. Native EDS/Camel-Ordner werden erst über den Linux-Expunge-Pfad endgültig geleert.",
                "Papierkorb geleert",
            ),
            (
                "expunge",
                "Papierkorb endgültig leeren?",
                "Der lokale app-eigene Papierkorb wird geleert. Für native EDS/Camel-Ordner ist der Linux-Expunge-Pfad erforderlich.",
                "Papierkorb endgültig geleert",
            ),
        ] {
            let action = gio::SimpleAction::new(action_name, None);
            let heading = heading.to_string();
            let body = body.to_string();
            let success = success.to_string();
            action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    let win_for_action = win.clone();
                    let success = success.clone();
                    crate::dialogs::run_destructive_action(
                        &win,
                        &heading,
                        &body,
                        "Leeren",
                        move || {
                            if let Some(mail_page) = win_for_action.mail_page() {
                                mail_page.empty_trash_async(&success);
                            } else {
                                win_for_action
                                    .add_toast(&adw::Toast::new("Mailansicht ist nicht verfügbar"));
                            }
                        },
                    );
                }
            ));
            self.add_action(&action);
        }

        let action_search = gio::SimpleAction::new("search", None);
        action_search.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| window.focus_active_search()
        ));
        self.add_action(&action_search);

        for (action_name, offset) in [("mail-next", 1), ("mail-prev", -1)] {
            let action = gio::SimpleAction::new(action_name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| {
                    if let Some(mail_page) = window
                        .imp()
                        .view_stack
                        .borrow()
                        .as_ref()
                        .and_then(|stack| stack.child_by_name("mail"))
                        .and_then(|widget| widget.downcast::<MailPage>().ok())
                    {
                        mail_page.select_relative_message(offset);
                    }
                }
            ));
            self.add_action(&action);
        }

        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            let window = self.downgrade();
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(window) = window.upgrade() {
                    window.set_mail_actions_enabled(
                        stack
                            .visible_child_name()
                            .is_some_and(|view_name| view_name == "mail"),
                    );
                }
            });
            self.set_mail_actions_enabled(
                view_stack
                    .visible_child_name()
                    .is_some_and(|view_name| view_name == "mail"),
            );
        }

        if let Some(mail_page) = self.mail_page() {
            let window = self.downgrade();
            mail_page.connect_selected_message_notify(move |has_selection| {
                if let Some(window) = window.upgrade() {
                    let is_mail_view = window
                        .imp()
                        .view_stack
                        .borrow()
                        .as_ref()
                        .and_then(|view_stack| view_stack.visible_child_name())
                        .is_some_and(|view_name| view_name == "mail");
                    window.set_message_actions_enabled(is_mail_view && has_selection);
                }
            });
        }
    }
}
