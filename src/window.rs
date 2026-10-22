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
//           │   ├── [end] GtkButton (New Mail)
//           │   └── [end] GtkMenuButton (primary menu)
//           └── [content] AdwViewStack
//               ├── "mail"     → MailPage (3-column split)
//               ├── "calendar" → CalendarPage
//               └── "contacts" → ContactsPage

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::Rc;

use crate::action_contract::{ACTIONS, ActionScope, MenuPath, menu_actions};
use crate::application::MeridianApplication;
use crate::dialogs::{ReminderDialog, ReminderDialogAction};
use crate::i18n::{gettext, ngettext};
use crate::services::AppSettings;
use crate::services::mail_print_service::MailPrintAction;
use crate::services::mail_transport_service::{
    MAIL_STOP_ACTION_ID, MailTransportAction, MailTransportMode, MailTransportOperation,
    MailTransportService, MailTransportSnapshot, MailTransportStartError, MailTransportState,
    WORK_OFFLINE_ACTION_ID, WORK_ONLINE_ACTION_ID,
};
use crate::services::reminder_service::{
    ReminderComponentKind, ReminderComponentPresence, ReminderDelivery, ReminderIdentity,
    component_presence, parse_reminder_deep_link,
};
use crate::services::search_engine::SearchTarget;
use crate::views::accessibility::set_accessible_label;
use crate::views::{CalendarPage, CalendarView, ContactsPage, MailPage, MemosPage, TasksPage};
use std::time::Instant;

pub const MIN_SIDEBAR_WIDTH: i32 = 240;
pub const MAX_SIDEBAR_WIDTH: i32 = 450;

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct MeridianWindow {
        pub settings: RefCell<Option<AppSettings>>,
        pub header_bar: RefCell<Option<adw::HeaderBar>>,
        pub sidebar_header: RefCell<Option<adw::HeaderBar>>,
        pub sidebar_title: RefCell<Option<adw::WindowTitle>>,
        pub sidebar_close_button: RefCell<Option<gtk::Button>>,
        pub restore_sidebar_button: RefCell<Option<gtk::Button>>,
        pub right_sidebar_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub agenda_preview_toggle: RefCell<Option<gtk::ToggleButton>>,
        pub mail_view_options_button: RefCell<Option<adw::SplitButton>>,
        pub contacts_view_options_button: RefCell<Option<adw::SplitButton>>,
        pub calendar_year_view_options_button: RefCell<Option<adw::SplitButton>>,
        pub mail_filter_options_button: RefCell<Option<gtk::MenuButton>>,
        pub sidebar_visible: Cell<bool>,
        pub sidebar_animation: RefCell<Option<adw::TimedAnimation>>,
        pub search_button: RefCell<Option<gtk::ToggleButton>>,
        pub compose_button: RefCell<Option<gtk::Button>>,
        pub view_stack: RefCell<Option<adw::ViewStack>>,
        pub toast_overlay: adw::ToastOverlay,
        pub status_label: RefCell<Option<gtk::Label>>,
        pub status_progress: RefCell<Option<gtk::ProgressBar>>,
        pub online_toggle: RefCell<Option<gtk::Button>>,
        pub debug_revealer: RefCell<Option<gtk::Revealer>>,
        pub statusbar_box: RefCell<Option<gtk::Box>>,
        pub(crate) transport_service: MailTransportService,
        pub transport_generation: Cell<u64>,
        pub transport_queued_items: Cell<usize>,
        pub transport_status_source: RefCell<Option<glib::SourceId>>,
        pub reminder_dialog: RefCell<Option<ReminderDialog>>,
        pub component_open_generation: Cell<u64>,
        pub component_open_in_progress: Cell<bool>,
        pub component_open_target: RefCell<Option<ReminderIdentity>>,
        pub last_component_open: RefCell<Option<(ReminderIdentity, Instant)>>,
        pub(crate) search_route_state: Cell<SearchRouteState>,
        pub search_route_source: RefCell<Option<glib::SourceId>>,
        pub back_button: RefCell<Option<gtk::Button>>,
        pub view_switcher_bar: RefCell<Option<adw::ViewSwitcherBar>>,
        pub view_switcher: RefCell<Option<adw::ViewSwitcher>>,
        pub window_title: RefCell<Option<adw::WindowTitle>>,
        pub mail_stack_page: RefCell<Option<adw::ViewStackPage>>,
        pub mail_switcher_badges: RefCell<Vec<gtk::Label>>,
        pub is_mobile: Cell<bool>,
        pub is_tablet: Cell<bool>,
        pub is_compact: Cell<bool>,
        pub is_medium: Cell<bool>,
        pub(crate) account_file_monitors: RefCell<Vec<gio::FileMonitor>>,
        pub(crate) account_bus_subscriptions: RefCell<Vec<gio::SignalSubscriptionId>>,
        pub(crate) account_reload_source: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MeridianWindow {
        const NAME: &'static str = "MeridianWindow";
        type Type = super::MeridianWindow;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for MeridianWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let window = self.obj();
            let settings = AppSettings::try_new();
            self.settings.replace(settings.clone());
            let initial_sidebar_vis = settings.as_ref().map(|s| s.mail_show_sidebar()).unwrap_or(true);
            self.sidebar_visible.set(initial_sidebar_vis);

            window.set_title(Some("Meridian"));
            if let Some(settings) = settings.as_ref() {
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
            window.setup_account_monitoring();

            if std::env::var("MERIDIAN_OPEN_DEBUG_CONSOLE").or_else(|_| std::env::var("EVOLUTION_OPEN_DEBUG_CONSOLE")).is_ok() {
                window.set_debug_console_open(true);
            }

            tracing::debug!("Main window constructed");
        }
    }

    impl WidgetImpl for MeridianWindow {
        fn realize(&self) {
            self.parent_realize();
        }
    }
    impl WindowImpl for MeridianWindow {
        fn close_request(&self) -> glib::Propagation {
            let window = self.obj();
            if let Some(source) = self.account_reload_source.borrow_mut().take() {
                source.remove();
            }
            for monitor in self.account_file_monitors.borrow_mut().drain(..) {
                monitor.cancel();
            }
            if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
                for sub in self.account_bus_subscriptions.borrow_mut().drain(..) {
                    bus.signal_unsubscribe(sub);
                }
            }
            window.imp().transport_service.stop();
            if let Some(source) = self.transport_status_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(mail_page) = window.mail_page() {
                mail_page.shutdown_print();
            }
            window.execute_exit_purge_and_expunge();
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
    impl ApplicationWindowImpl for MeridianWindow {}
    impl AdwApplicationWindowImpl for MeridianWindow {}
}

// ─── Public API ─────────────────────────────────────────────────────────────

glib::wrapper! {
    pub struct MeridianWindow(ObjectSubclass<imp::MeridianWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow,
                 gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap,
                    gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native,
                    gtk::Root, gtk::ShortcutManager;
}

#[cfg(test)]
pub(crate) static GTK_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn gtk_test_main_context() -> Option<glib::MainContext> {
    if !gtk::is_initialized() {
        if gtk::init().is_err() {
            eprintln!("skipping GTK integration test: no display is available");
            return None;
        }
        if gdk::Display::default().is_none() {
            eprintln!("skipping GTK integration test: no display is available");
            return None;
        }
        if !adw::is_initialized() {
            let _ = adw::init();
        }
    } else if !gtk::is_initialized_main_thread() {
        eprintln!("skipping GTK integration test: GTK is initialized on another thread");
        return None;
    }
    Some(glib::MainContext::default())
}

const MAX_SEARCH_ROUTE_ATTEMPTS: u8 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchRoute {
    Mail,
    Calendar,
    Tasks,
    Memos,
    Contacts,
}

impl SearchRoute {
    fn page_name(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Tasks => "tasks",
            Self::Memos => "memos",
            Self::Contacts => "contacts",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchRouteError {
    MissingTarget,
    EmptyMailId,
    EmptyComponentUid,
    EmptyContactUid,
}

fn search_route_for_target(target: Option<&SearchTarget>) -> Result<SearchRoute, SearchRouteError> {
    match target {
        None => Err(SearchRouteError::MissingTarget),
        Some(SearchTarget::Mail { local_id }) if local_id.is_empty() => {
            Err(SearchRouteError::EmptyMailId)
        }
        Some(SearchTarget::Mail { .. }) => Ok(SearchRoute::Mail),
        Some(SearchTarget::Event { component_uid, .. }) if component_uid.is_empty() => {
            Err(SearchRouteError::EmptyComponentUid)
        }
        Some(SearchTarget::Event { .. }) => Ok(SearchRoute::Calendar),
        Some(SearchTarget::Task { component_uid, .. }) if component_uid.is_empty() => {
            Err(SearchRouteError::EmptyComponentUid)
        }
        Some(SearchTarget::Task { .. }) => Ok(SearchRoute::Tasks),
        Some(SearchTarget::Memo { component_uid, .. }) if component_uid.is_empty() => {
            Err(SearchRouteError::EmptyComponentUid)
        }
        Some(SearchTarget::Memo { .. }) => Ok(SearchRoute::Memos),
        Some(SearchTarget::Contact { contact_uid, .. }) if contact_uid.is_empty() => {
            Err(SearchRouteError::EmptyContactUid)
        }
        Some(SearchTarget::Contact { .. }) => Ok(SearchRoute::Contacts),
    }
}

fn search_route_error_message(error: SearchRouteError) -> String {
    match error {
        SearchRouteError::MissingTarget => gettext("Global search match has no target"),
        SearchRouteError::EmptyMailId => gettext("Global mail match contains no local ID"),
        SearchRouteError::EmptyComponentUid => {
            gettext("Global calendar/task/memo match contains no component UID")
        }
        SearchRouteError::EmptyContactUid => gettext("Global contact match contains no contact UID"),
    }
}

fn next_search_route_generation(current_generation: u64) -> u64 {
    current_generation.wrapping_add(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SearchRouteState {
    generation: u64,
    destination: Option<&'static str>,
}

impl SearchRouteState {
    fn begin(&mut self, route: SearchRoute) -> u64 {
        self.generation = next_search_route_generation(self.generation);
        self.destination = Some(route.page_name());
        self.generation
    }

    fn invalidate_for_visible_page(&mut self, visible_page: Option<&str>) -> bool {
        if self
            .destination
            .is_some_and(|destination| visible_page != Some(destination))
        {
            self.generation = next_search_route_generation(self.generation);
            self.destination = None;
            true
        } else {
            false
        }
    }

    fn is_current(
        self,
        route_generation: u64,
        route: SearchRoute,
        visible_page: Option<&str>,
    ) -> bool {
        self.generation == route_generation
            && self.destination == Some(route.page_name())
            && visible_page == Some(route.page_name())
    }

    fn complete(&mut self, route_generation: u64) {
        if self.generation == route_generation {
            self.destination = None;
        }
    }
}

fn search_route_decision(
    state: SearchRouteState,
    route_generation: u64,
    route: SearchRoute,
    visible_page: Option<&str>,
    attempts: u8,
    found: bool,
) -> SearchRouteDecision {
    if !state.is_current(route_generation, route, visible_page) {
        SearchRouteDecision::RejectStale
    } else if found {
        SearchRouteDecision::Complete
    } else if attempts >= MAX_SEARCH_ROUTE_ATTEMPTS {
        SearchRouteDecision::Failed
    } else {
        SearchRouteDecision::Retry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchRouteDecision {
    RejectStale,
    Complete,
    Retry,
    Failed,
}

impl MeridianWindow {
    pub fn new(app: &MeridianApplication) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// Add an AdwToast notification to the window.
    pub fn add_toast(&self, toast: &adw::Toast) {
        self.imp().toast_overlay.add_toast(toast.clone());
    }

    pub fn set_active_view(&self, view_name: &str) {
        if let Some(cal) = self.calendar_page() {
            cal.dismiss_zoom_toast();
        }
        if let Some(stack) = self.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name(view_name);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view(view_name);
        }
    }

    pub fn search_button(&self) -> Option<gtk::ToggleButton> {
        self.imp().search_button.borrow().clone()
    }

    pub fn mail_filter_options_button(&self) -> Option<gtk::MenuButton> {
        self.imp().mail_filter_options_button.borrow().clone()
    }

    pub(crate) fn present_reminders(&self, deliveries: Vec<ReminderDelivery>, show_dialog: bool) {
        let dialog = if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
            dialog.clone()
        } else {
            let window_for_open = self.downgrade();
            let window_for_action = self.downgrade();
            let dialog = ReminderDialog::new(
                Some(self),
                move |delivery| {
                    let Some(window) = window_for_open.upgrade() else {
                        return false;
                    };
                    let Ok(identity) = delivery.identity() else {
                        return false;
                    };
                    window.open_component(identity)
                },
                move |delivery, action| {
                    let Some(window) = window_for_action.upgrade() else {
                        return false;
                    };
                    let Some(application) = window.application().and_then(|application| {
                        application.downcast::<MeridianApplication>().ok()
                    }) else {
                        return false;
                    };
                    let Ok(target) = delivery.action_target() else {
                        return false;
                    };
                    match action {
                        ReminderDialogAction::Snooze(seconds) => {
                            application.apply_reminder_action_from_dialog(&target, Some(seconds))
                        }
                        ReminderDialogAction::Dismiss => {
                            application.apply_reminder_action_from_dialog(&target, None)
                        }
                    }
                },
            );
            self.imp().reminder_dialog.replace(Some(dialog.clone()));
            dialog
        };
        dialog.update_deliveries(&deliveries, show_dialog);
    }

    pub(crate) fn present_existing_reminders(&self) {
        if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
            dialog.present_existing();
        } else {
            self.present_reminders(Vec::new(), false);
            if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
                dialog.present_existing();
            }
        }
    }

    pub(crate) fn finish_reminder_action(
        &self,
        target: &str,
        success: bool,
        error: Option<String>,
    ) {
        if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
            dialog.finish_action(target, success, error.as_deref());
        }
        if !success {
            if let Some(error) = error {
                self.add_toast(&adw::Toast::new(&error));
            }
        }
    }

    fn set_component_open_action_sensitivity(&self, enabled: bool) {
        for name in ["open-event", "open-task", "open-memo"] {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
    }

    pub(crate) fn open_component(&self, identity: ReminderIdentity) -> bool {
        if self
            .imp()
            .component_open_target
            .borrow()
            .as_ref()
            .is_some_and(|target| {
                *target == identity && self.imp().component_open_in_progress.get()
            })
        {
            return true;
        }
        if self
            .imp()
            .last_component_open
            .borrow()
            .as_ref()
            .is_some_and(|(target, opened_at)| {
                *target == identity && opened_at.elapsed() < std::time::Duration::from_millis(750)
            })
        {
            self.finish_reminder_open(identity, true, None);
            return true;
        }

        if self.imp().component_open_in_progress.get() {
            let superseded = self.imp().component_open_target.borrow_mut().take();
            if let Some(superseded) = superseded {
                self.finish_reminder_open(
                    superseded,
                    false,
                    Some(gettext("Open replaced by a newer reminder action")),
                );
            }
        }

        let page_name = match identity.kind {
            ReminderComponentKind::Event => "calendar",
            ReminderComponentKind::Task => "tasks",
            ReminderComponentKind::Memo => "memos",
            ReminderComponentKind::Contact => "contacts",
        };
        let generation = self.imp().component_open_generation.get().wrapping_add(1);
        self.imp().component_open_generation.set(generation);
        self.imp().component_open_in_progress.set(true);
        self.imp()
            .component_open_target
            .replace(Some(identity.clone()));
        self.set_component_open_action_sensitivity(false);
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name(page_name);
        }

        let window = self.downgrade();
        let mut attempts = 0u8;
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.imp().component_open_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let found = match identity.kind {
                ReminderComponentKind::Event => window.calendar_page().is_some_and(|page| {
                    page.open_search_result(&identity.source_uid, &identity.component_uid)
                }),
                ReminderComponentKind::Task => window.tasks_page().is_some_and(|page| {
                    page.open_search_result(&identity.source_uid, &identity.component_uid)
                }),
                ReminderComponentKind::Memo => window.memos_page().is_some_and(|page| {
                    page.select_search_result(&identity.source_uid, &identity.component_uid)
                }),
                ReminderComponentKind::Contact => window.contacts_page().is_some_and(|page| {
                    page.select_search_result(&identity.source_uid, &identity.component_uid)
                }),
            };
            if found {
                window.imp().component_open_in_progress.set(false);
                window.imp().component_open_target.replace(None);
                window
                    .imp()
                    .last_component_open
                    .replace(Some((identity.clone(), Instant::now())));
                window.set_component_open_action_sensitivity(true);
                window.finish_reminder_open(identity.clone(), true, None);
                return glib::ControlFlow::Break;
            }

            attempts = attempts.saturating_add(1);
            if attempts >= 40 {
                window.imp().component_open_in_progress.set(false);
                window.imp().component_open_target.replace(None);
                window.set_component_open_action_sensitivity(true);
                window.report_component_open_failure(identity.clone(), generation);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        true
    }

    fn report_component_open_failure(&self, identity: ReminderIdentity, generation: u64) {
        let identity_for_worker = identity.clone();
        let local_calendar_directory =
            AppSettings::try_new().map(|settings| settings.local_calendar_directory());
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let snapshot = crate::services::reminder_service::calendar_store_for_worker(
                local_calendar_directory,
            )
            .load_snapshot();
            let status = component_presence(&snapshot, &identity_for_worker);
            let _ = sender.send_blocking(status);
        });
        let window = self.downgrade();
        glib::spawn_future_local(async move {
            let status = match receiver.recv().await {
                Ok(status) => status,
                Err(error) => {
                    if let Some(window) = window.upgrade() {
                        if window.imp().component_open_generation.get() != generation {
                            return;
                        }
                        window.finish_reminder_open(
                            identity.clone(),
                            false,
                            Some(
                                gettext("Could not check reminder source: {error}")
                                    .replace("{error}", &error.to_string()),
                            ),
                        );
                    }
                    return;
                }
            };
            let Some(window) = window.upgrade() else {
                return;
            };
            if window.imp().component_open_generation.get() != generation {
                return;
            }
            let message = match status {
                ReminderComponentPresence::MissingSource => {
                    gettext("Reminder source is unavailable or was removed")
                }
                ReminderComponentPresence::MissingComponent => {
                    gettext("Reminder component is unavailable or was modified")
                }
                ReminderComponentPresence::Found => {
                    gettext("Component could not be opened after loading")
                }
            };
            window.finish_reminder_open(identity, false, Some(message));
        });
    }

    fn finish_reminder_open(
        &self,
        identity: ReminderIdentity,
        success: bool,
        error: Option<String>,
    ) {
        if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
            dialog.finish_open(&identity, success, error.as_deref());
        }
        if !success {
            if let Some(error) = error {
                self.add_toast(&adw::Toast::new(&error));
            }
        }
    }

    pub(crate) fn mail_page(&self) -> Option<MailPage> {
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

    pub(crate) fn switch_to_view(&self, view_name: &str) {
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name(view_name);
        }
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

    pub(crate) fn show_calendar_date(&self, date: chrono::NaiveDate) {
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("calendar");
        }
        if let Some(calendar_page) = self.calendar_page() {
            calendar_page.navigate_to_date_contextual(date);
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
        let window = self.downgrade();
        dialog.connect_closed(move |_| {
            if let Some(window) = window.upgrade() {
                window.reload_mail_accounts();
            }
        });
        dialog.present(Some(self));
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

    pub fn active_view_name(&self) -> String {
        self.imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|stack| stack.visible_child_name())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "mail".to_string())
    }

    pub fn active_view_outer_paned(&self) -> Option<gtk::Paned> {
        match self.active_view_name().as_str() {
            "mail" => self.mail_page().map(|p| p.outer_paned().clone()),
            "calendar" => self.calendar_page().and_then(|p| p.outer_paned()),
            "contacts" => self.contacts_page().and_then(|p| p.outer_paned()),
            "tasks" => self.tasks_page().and_then(|p| p.outer_paned()),
            "memos" => self.memos_page().and_then(|p| p.outer_paned()),
            _ => None,
        }
    }

    pub fn is_sidebar_visible_for_active_view(&self) -> bool {
        self.imp().sidebar_visible.get()
    }

    pub fn is_sidebar_visible(&self) -> bool {
        self.imp().sidebar_visible.get()
    }

    pub fn sync_sidebar_state_for_current_view(&self) {
        let view_name = self.active_view_name();
        let is_vis = self.is_sidebar_visible_for_active_view();

        if self.imp().is_mobile.get() {
            if let Some(mail) = self.mail_page() {
                mail.sync_mail_layout_visibility();
            }
            if let Some(cal) = self.calendar_page() {
                cal.sync_calendar_layout_visibility();
            }
            if let Some(contacts) = self.contacts_page() {
                contacts.sync_contacts_layout_visibility();
            }
            if let Some(tasks) = self.tasks_page() {
                tasks.sync_tasks_layout_visibility();
            }
            if let Some(memos) = self.memos_page() {
                memos.sync_memos_layout_visibility();
            }
        } else {
            match view_name.as_str() {
                "mail" => {
                    if let Some(mail) = self.mail_page() {
                        mail.set_sidebar_visible(is_vis);
                    }
                }
                "calendar" => {
                    if let Some(cal) = self.calendar_page() {
                        cal.set_sidebar_visible(is_vis);
                    }
                }
                "contacts" => {
                    if let Some(contacts) = self.contacts_page() {
                        contacts.set_sidebar_visible(is_vis);
                    }
                }
                "tasks" => {
                    if let Some(tasks) = self.tasks_page() {
                        tasks.set_sidebar_visible(is_vis);
                    }
                }
                "memos" => {
                    if let Some(memos) = self.memos_page() {
                        memos.set_sidebar_visible(is_vis);
                    }
                }
                _ => {}
            }
        }

        let title_str = match view_name.as_str() {
            "mail" => self
                .mail_page()
                .map(|m| m.active_folder_display_title())
                .unwrap_or_else(|| crate::i18n::gettext("Inbox")),
            "calendar" => crate::i18n::gettext("Calendar"),
            "contacts" => crate::i18n::gettext("Contacts"),
            "tasks" => crate::i18n::gettext("Tasks"),
            "memos" => crate::i18n::gettext("Memos"),
            _ => String::new(),
        };
        self.update_sidebar_title(&title_str);

        if self.imp().is_mobile.get() {
            if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                sb_h.set_visible(false);
                sb_h.set_size_request(0, -1);
            }
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                res_btn.set_visible(false);
            }
            if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                hb.set_show_start_title_buttons(true);
            }
        } else {
            let saved_pos = self.sidebar_position_for_view(&self.active_view_name());
            let cur_pos = if is_vis {
                self.active_view_outer_paned()
                    .map(|p| p.position())
                    .unwrap_or(saved_pos)
                    .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
            } else {
                saved_pos
            };

            if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                sb_h.set_visible(is_vis);
                sb_h.set_show_start_title_buttons(is_vis);
                if is_vis {
                    sb_h.set_size_request(cur_pos, -1);
                }
            }
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                res_btn.set_visible(!is_vis);
                res_btn.set_tooltip_text(Some(&crate::i18n::gettext("Show Sidebar (F9)")));
                set_accessible_label(res_btn, &crate::i18n::gettext("Show Sidebar"));
            }
            if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                hb.set_show_start_title_buttons(!is_vis);
            }
            if let Some(title) = self.imp().sidebar_title.borrow().as_ref() {
                title.set_visible(is_vis);
            }
            if let Some(btn) = self.imp().sidebar_close_button.borrow().as_ref() {
                btn.set_visible(is_vis);
            }
        }

        if let Some(action) = self.lookup_action("toggle-sidebar").and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
            if action.state().and_then(|v| v.get::<bool>()) != Some(is_vis) {
                action.set_state(&is_vis.to_variant());
            }
        }
    }

    pub fn is_active_view_right_panel_visible(&self) -> bool {
        match self.active_view_name().as_str() {
            "mail" => self.mail_page().map(|p| p.is_todo_bar_visible()).unwrap_or(false),
            "calendar" => self.calendar_page().map(|p| p.is_tag_vpane_visible()).unwrap_or(true),
            "contacts" => self.contacts_page().map(|p| p.is_preview_visible()).unwrap_or(true),
            "tasks" => self.tasks_page().map(|p| p.is_preview_visible()).unwrap_or(true),
            "memos" => self.memos_page().map(|p| p.is_preview_visible()).unwrap_or(true),
            _ => false,
        }
    }

    pub fn set_active_view_right_panel_visible(&self, visible: bool) {
        match self.active_view_name().as_str() {
            "mail" => {
                if let Some(p) = self.mail_page() {
                    p.set_todo_bar_visible(visible);
                }
            }
            "calendar" => {
                if let Some(p) = self.calendar_page() {
                    p.set_tag_vpane_visible(visible);
                }
            }
            "contacts" => {
                if let Some(p) = self.contacts_page() {
                    p.set_preview_visible(visible);
                }
            }
            "tasks" => {
                if let Some(p) = self.tasks_page() {
                    p.set_preview_visible(visible);
                }
            }
            "memos" => {
                if let Some(p) = self.memos_page() {
                    p.set_preview_visible(visible);
                }
            }
            _ => {}
        }
        self.sync_right_sidebar_toggle();
    }

    pub fn toggle_active_view_right_panel(&self) {
        let current = self.is_active_view_right_panel_visible();
        self.set_active_view_right_panel_visible(!current);
    }

    pub fn is_toolbar_visible_for_active_view(&self) -> bool {
        match self.active_view_name().as_str() {
            "mail" => self
                .mail_page()
                .map(|p| p.is_toolbar_visible())
                .unwrap_or_else(|| {
                    AppSettings::try_new()
                        .map(|s| s.mail_show_toolbar())
                        .unwrap_or(false)
                }),
            "contacts" => self
                .contacts_page()
                .map(|p| p.is_toolbar_visible())
                .unwrap_or_else(|| {
                    AppSettings::try_new()
                        .map(|s| s.contact_show_toolbar())
                        .unwrap_or(false)
                }),
            "tasks" => self
                .tasks_page()
                .map(|p| p.is_toolbar_visible())
                .unwrap_or_else(|| {
                    AppSettings::try_new()
                        .map(|s| s.task_show_toolbar())
                        .unwrap_or(false)
                }),
            "memos" => self
                .memos_page()
                .map(|p| p.is_toolbar_visible())
                .unwrap_or_else(|| {
                    AppSettings::try_new()
                        .map(|s| s.memo_show_toolbar())
                        .unwrap_or(false)
                }),
            "calendar" => true,
            _ => false,
        }
    }

    pub fn set_active_view_toolbar_visible(&self, visible: bool) {
        match self.active_view_name().as_str() {
            "mail" => {
                if let Some(p) = self.mail_page() {
                    p.set_toolbar_visible(visible);
                } else if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_show_toolbar(visible);
                }
            }
            "contacts" => {
                if let Some(p) = self.contacts_page() {
                    p.set_toolbar_visible(visible);
                } else if let Some(settings) = AppSettings::try_new() {
                    settings.set_contact_show_toolbar(visible);
                }
            }
            "tasks" => {
                if let Some(p) = self.tasks_page() {
                    p.set_toolbar_visible(visible);
                } else if let Some(settings) = AppSettings::try_new() {
                    settings.set_task_show_toolbar(visible);
                }
            }
            "memos" => {
                if let Some(p) = self.memos_page() {
                    p.set_toolbar_visible(visible);
                } else if let Some(settings) = AppSettings::try_new() {
                    settings.set_memo_show_toolbar(visible);
                }
            }
            "calendar" => {}
            _ => {}
        }
        self.sync_toolbar_state_for_current_view();
    }

    pub fn set_all_views_toolbar_visible(&self, visible: bool) {
        if let Some(p) = self.mail_page() {
            p.set_toolbar_visible(visible);
        }
        if let Some(p) = self.contacts_page() {
            p.set_toolbar_visible(visible);
        }
        if let Some(p) = self.tasks_page() {
            p.set_toolbar_visible(visible);
        }
        if let Some(p) = self.memos_page() {
            p.set_toolbar_visible(visible);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_mail_show_toolbar(visible);
            settings.set_contact_show_toolbar(visible);
            settings.set_task_show_toolbar(visible);
            settings.set_memo_show_toolbar(visible);
        }
        self.sync_toolbar_state_for_current_view();
    }

    pub fn sync_toolbar_state_for_current_view(&self) {
        let is_vis = self.is_toolbar_visible_for_active_view();
        if let Some(action) = self
            .lookup_action("toggle-toolbar")
            .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
        {
            if action.state().and_then(|v| v.get::<bool>()) != Some(is_vis) {
                action.set_state(&is_vis.to_variant());
            }
        }
    }

    pub fn sync_right_sidebar_toggle(&self) {
        if let Some(toggle) = self.imp().right_sidebar_toggle.borrow().as_ref() {
            if self.imp().is_mobile.get() {
                toggle.set_visible(false);
                if let Some(agenda_btn) = self.imp().agenda_preview_toggle.borrow().as_ref() {
                    agenda_btn.set_visible(false);
                }
                if let Some(year_btn) = self.imp().calendar_year_view_options_button.borrow().as_ref() {
                    year_btn.set_visible(false);
                }
                return;
            }
            toggle.set_visible(true);

            let (tooltip, label) = match self.active_view_name().as_str() {
                "mail" => (
                    crate::i18n::gettext("Toggle To Do bar for today"),
                    crate::i18n::gettext("To Do bar for today"),
                ),
                "calendar" => (
                    crate::i18n::gettext("Toggle tasks and memos bar (Ctrl+M)"),
                    crate::i18n::gettext("Tasks and memos bar"),
                ),
                "contacts" => (
                    crate::i18n::gettext("Toggle contact preview (Ctrl+M)"),
                    crate::i18n::gettext("Contact preview"),
                ),
                "tasks" => (
                    crate::i18n::gettext("Toggle task preview (Ctrl+M)"),
                    crate::i18n::gettext("Task preview"),
                ),
                "memos" => (
                    crate::i18n::gettext("Toggle memo preview (Ctrl+M)"),
                    crate::i18n::gettext("Memo preview"),
                ),
                _ => (
                    crate::i18n::gettext("Toggle right side panel"),
                    crate::i18n::gettext("Right side panel"),
                ),
            };

            toggle.set_tooltip_text(Some(&tooltip));
            set_accessible_label(toggle, &label);

            let is_vis = self.is_active_view_right_panel_visible();
            if toggle.is_active() != is_vis {
                toggle.set_active(is_vis);
            }
        }

        if let Some(agenda_btn) = self.imp().agenda_preview_toggle.borrow().as_ref() {
            let is_agenda_view = self.active_view_name().as_str() == "calendar"
                && self
                    .calendar_page()
                    .map(|p| p.current_calendar_view() == CalendarView::Agenda)
                    .unwrap_or(false);
            let should_show = is_agenda_view && !self.imp().is_mobile.get();
            agenda_btn.set_visible(should_show);
            if should_show {
                if let Some(page) = self.calendar_page() {
                    let is_vis = page.is_agenda_detail_visible();
                    if agenda_btn.is_active() != is_vis {
                        agenda_btn.set_active(is_vis);
                    }
                }
            }
        }

        if self.imp().calendar_year_view_options_button.borrow().is_none() {
            if let Some(cal) = self.calendar_page() {
                self.attach_calendar_year_view_options_button(&cal);
            }
        }

        if let Some(year_btn) = self.imp().calendar_year_view_options_button.borrow().as_ref() {
            let is_year_view = self.active_view_name().as_str() == "calendar"
                && self
                    .calendar_page()
                    .map(|p| p.current_calendar_view() == CalendarView::Year)
                    .unwrap_or(false);
            let should_show = is_year_view && !self.imp().is_mobile.get();
            year_btn.set_visible(should_show);
        }
    }

    pub fn back_button(&self) -> Option<gtk::Button> {
        self.imp().back_button.borrow().clone()
    }

    pub fn view_switcher_bar(&self) -> Option<adw::ViewSwitcherBar> {
        self.imp().view_switcher_bar.borrow().clone()
    }

    pub fn mail_view_options_button(&self) -> Option<adw::SplitButton> {
        self.imp().mail_view_options_button.borrow().clone()
    }

    pub fn contacts_view_options_button(&self) -> Option<adw::SplitButton> {
        self.imp().contacts_view_options_button.borrow().clone()
    }

    pub fn calendar_year_view_options_button(&self) -> Option<adw::SplitButton> {
        self.imp().calendar_year_view_options_button.borrow().clone()
    }

    pub fn attach_calendar_year_view_options_button(&self, page: &CalendarPage) {
        if self.imp().calendar_year_view_options_button.borrow().is_some() {
            return;
        }
        let Some(header_bar) = self.imp().header_bar.borrow().clone() else {
            return;
        };
        if let Some(view_btn) = page.year_view_options_split_button() {
            let is_initially_year = self.active_view_name().as_str() == "calendar"
                && page.current_calendar_view() == CalendarView::Year
                && !self.imp().is_mobile.get();
            view_btn.set_visible(is_initially_year);
            header_bar.pack_end(&view_btn);
            self.imp()
                .calendar_year_view_options_button
                .replace(Some(view_btn));
        }
    }

    pub fn attach_contacts_view_options_button(&self, page: &ContactsPage) {
        if self.imp().contacts_view_options_button.borrow().is_some() {
            return;
        }
        let Some(header_bar) = self.imp().header_bar.borrow().clone() else {
            return;
        };
        if let Some(view_btn) = page.view_options_split_button() {
            let view_stack = self.imp().view_stack.borrow().clone();
            let is_initially_contacts = view_stack
                .as_ref()
                .and_then(|vs| vs.visible_child_name())
                .as_deref()
                == Some("contacts");
            view_btn.set_visible(is_initially_contacts);
            header_bar.pack_end(&view_btn);
            self.imp()
                .contacts_view_options_button
                .replace(Some(view_btn.clone()));
            if let Some(stack) = view_stack.as_ref() {
                let btn_weak = view_btn.downgrade();
                stack.connect_visible_child_name_notify(move |stack| {
                    if let Some(btn) = btn_weak.upgrade() {
                        let is_contacts = stack.visible_child_name().as_deref() == Some("contacts");
                        btn.set_visible(is_contacts);
                    }
                });
            }
        }
    }

    pub(crate) fn can_navigate_back(&self) -> bool {
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            match view_stack.visible_child_name().as_deref() {
                Some("mail") => self.mail_page().map(|p| p.can_navigate_back()).unwrap_or(false),
                Some("calendar") => self.calendar_page().map(|p| p.can_navigate_back()).unwrap_or(false),
                Some("tasks") => self.tasks_page().map(|p| p.can_navigate_back()).unwrap_or(false),
                Some("contacts") => self.contacts_page().map(|p| p.can_navigate_back()).unwrap_or(false),
                Some("memos") => self.memos_page().map(|p| p.can_navigate_back()).unwrap_or(false),
                _ => false,
            }
        } else {
            false
        }
    }

    pub(crate) fn sync_back_button(&self) {
        let is_mobile = self.imp().is_mobile.get();
        let can_back = self.can_navigate_back();

        if let Some(back_btn) = self.imp().back_button.borrow().as_ref() {
            back_btn.set_visible(can_back);
        }

        if let Some(search_btn) = self.imp().search_button.borrow().as_ref() {
            if is_mobile {
                search_btn.set_visible(!can_back);
            } else {
                search_btn.set_visible(true);
            }
        }

        if let Some(compose_btn) = self.imp().compose_button.borrow().as_ref() {
            if is_mobile {
                compose_btn.set_visible(!can_back);
            } else {
                compose_btn.set_visible(true);
            }
        }

        if is_mobile {
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                let show_mobile_sidebar_toggle = self.active_view_name() == "mail"
                    && self.mail_page().map(|p| p.drill_down_level() == crate::views::MailDrillDownLevel::Messages).unwrap_or(false);
                res_btn.set_visible(show_mobile_sidebar_toggle);
            }

            if let (Some(view_stack), Some(window_title)) = (
                self.imp().view_stack.borrow().as_ref(),
                self.imp().window_title.borrow().as_ref(),
            ) {
                match view_stack.visible_child_name().as_deref() {
                    Some("mail") => {
                        if let Some(mail) = self.mail_page() {
                            match mail.drill_down_level() {
                                crate::views::MailDrillDownLevel::Folders => {
                                    window_title.set_title(&crate::i18n::gettext("Folders & Accounts"));
                                    window_title.set_subtitle(&crate::i18n::gettext("Mail"));
                                }
                                crate::views::MailDrillDownLevel::Messages => {
                                    let mut folder = mail
                                        .selected_folder_display_name()
                                        .unwrap_or_else(|| crate::i18n::gettext("Inbox"));
                                    let show_counter = AppSettings::try_new()
                                        .map(|s| s.show_window_title_unread_counter())
                                        .unwrap_or(true);
                                    let count = mail.total_inbox_unread_count();
                                    if show_counter && count > 0 {
                                        folder = format!("{folder} ({count})");
                                    }
                                    window_title.set_title(&folder);
                                    window_title.set_subtitle(&crate::i18n::gettext("Messages"));
                                }
                                crate::views::MailDrillDownLevel::Viewer => {
                                    let subj = mail
                                        .selected_message_subject()
                                        .unwrap_or_else(|| crate::i18n::gettext("Email"));
                                    window_title.set_title(&subj);
                                    window_title.set_subtitle(&crate::i18n::gettext("Message"));
                                }
                            }
                        }
                    }
                    Some("calendar") => {
                        if let Some(cal) = self.calendar_page() {
                            let period = cal.period_label_text();
                            let view_name = cal.current_view_title();
                            window_title.set_title(&period);
                            window_title.set_subtitle(&view_name);
                        } else {
                            window_title.set_title(&crate::i18n::gettext("Calendar"));
                            window_title.set_subtitle("");
                        }
                    }
                    Some("contacts") => {
                        if let Some(contacts) = self.contacts_page() {
                            if contacts.is_detail_shown() {
                                let name = contacts
                                    .selected_contact_display_name()
                                    .unwrap_or_else(|| crate::i18n::gettext("Contact"));
                                window_title.set_title(&name);
                                window_title.set_subtitle(&crate::i18n::gettext("Contacts"));
                            } else {
                                window_title.set_title(&crate::i18n::gettext("Contacts"));
                                window_title.set_subtitle("");
                            }
                        } else {
                            window_title.set_title(&crate::i18n::gettext("Contacts"));
                            window_title.set_subtitle("");
                        }
                    }
                    Some("tasks") => {
                        if let Some(tasks) = self.tasks_page() {
                            if tasks.is_detail_shown() {
                                let title = tasks
                                    .selected_task_title()
                                    .unwrap_or_else(|| crate::i18n::gettext("Task"));
                                window_title.set_title(&title);
                                window_title.set_subtitle(&crate::i18n::gettext("Tasks"));
                            } else {
                                window_title.set_title(&crate::i18n::gettext("Tasks"));
                                window_title.set_subtitle("");
                            }
                        } else {
                            window_title.set_title(&crate::i18n::gettext("Tasks"));
                            window_title.set_subtitle("");
                        }
                    }
                    Some("memos") => {
                        if let Some(memos) = self.memos_page() {
                            if memos.is_detail_shown() {
                                let title = memos
                                    .selected_memo_title()
                                    .unwrap_or_else(|| crate::i18n::gettext("Memo"));
                                window_title.set_title(&title);
                                window_title.set_subtitle(&crate::i18n::gettext("Memos"));
                            } else {
                                window_title.set_title(&crate::i18n::gettext("Memos"));
                                window_title.set_subtitle("");
                            }
                        } else {
                            window_title.set_title(&crate::i18n::gettext("Memos"));
                            window_title.set_subtitle("");
                        }
                    }
                    _ => {
                        window_title.set_title("");
                        window_title.set_subtitle("");
                    }
                }
            }
        } else if self.imp().is_tablet.get() {
            if let (Some(view_stack), Some(window_title)) = (
                self.imp().view_stack.borrow().as_ref(),
                self.imp().window_title.borrow().as_ref(),
            ) {
                if view_stack.visible_child_name().as_deref() == Some("mail") {
                    if let Some(mail) = self.mail_page() {
                        if mail.drill_down_level() == crate::views::MailDrillDownLevel::Viewer {
                            let subj = mail
                                .selected_message_subject()
                                .unwrap_or_else(|| crate::i18n::gettext("Email"));
                            window_title.set_title(&subj);
                            window_title.set_subtitle("");
                        } else {
                            let mut folder = mail
                                .selected_folder_display_name()
                                .unwrap_or_else(|| crate::i18n::gettext("Inbox"));
                            let show_counter = AppSettings::try_new()
                                .map(|s| s.show_window_title_unread_counter())
                                .unwrap_or(true);
                            let count = mail.total_inbox_unread_count();
                            if show_counter && count > 0 {
                                folder = format!("{folder} ({count})");
                            }
                            window_title.set_title(&folder);
                            window_title.set_subtitle("");
                        }
                    } else {
                        window_title.set_title("");
                        window_title.set_subtitle("");
                    }
                } else {
                    window_title.set_title("");
                    window_title.set_subtitle("");
                }
            }
        } else if let Some(window_title) = self.imp().window_title.borrow().as_ref() {
            let show_counter = AppSettings::try_new()
                .map(|s| s.show_window_title_unread_counter())
                .unwrap_or(true);
            let count = self.mail_page().map(|p| p.total_inbox_unread_count()).unwrap_or(0);
            if show_counter && count > 0 {
                window_title.set_title(&format!("Meridian ({count})"));
            } else {
                window_title.set_title("Meridian");
            }
            window_title.set_subtitle("");
        }

        if let Some(header_bar) = self.imp().header_bar.borrow().as_ref() {
            if is_mobile || self.imp().is_tablet.get() {
                if let Some(window_title) = self.imp().window_title.borrow().as_ref() {
                    header_bar.set_title_widget(Some(window_title));
                }
            } else {
                let is_top = AppSettings::try_new()
                    .map(|s| s.switcher_location() == "top")
                    .unwrap_or(false);
                if is_top {
                    if let Some(switcher) = self.imp().view_switcher.borrow().as_ref() {
                        header_bar.set_title_widget(Some(switcher));
                    }
                } else if let Some(window_title) = self.imp().window_title.borrow().as_ref() {
                    header_bar.set_title_widget(Some(window_title));
                }
            }
        }
    }

    pub(crate) fn update_mail_unread_count(&self, count: u32) {
        let show_counter = AppSettings::try_new()
            .map(|s| s.show_window_title_unread_counter())
            .unwrap_or(true);
        let has_unread = show_counter && count > 0;
        if let Some(page) = self.imp().mail_stack_page.borrow().as_ref() {
            // Keep native badge_number at 0 so AdwIndicatorBin renders a clean minimal attention dot instead of numbers
            page.set_badge_number(0);
            page.set_needs_attention(has_unread);
        }

        let badge_text = if count > 999 {
            "999+".to_string()
        } else {
            count.to_string()
        };

        // Minimal indicator: The trailing pill label is kept hidden in favor of the attention dot on the icon
        for badge in self.imp().mail_switcher_badges.borrow().iter() {
            badge.set_label(&badge_text);
            badge.set_visible(false);
        }

        if show_counter && count > 0 {
            self.set_title(Some(&format!("Meridian ({count})")));
        } else {
            self.set_title(Some("Meridian"));
        }

        self.sync_back_button();
    }

    pub(crate) fn sync_unread_counter_and_title(&self) {
        let count = self.mail_page().map(|p| p.total_inbox_unread_count()).unwrap_or(0);
        self.update_mail_unread_count(count);
    }

    pub(crate) fn handle_navigate_back(&self) -> bool {
        if self.is_active_search_open() {
            self.close_active_search();
            self.sync_back_button();
            return true;
        }

        let handled = if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            match view_stack.visible_child_name().as_deref() {
                Some("mail") => self.mail_page().map(|p| p.navigate_back()).unwrap_or(false),
                Some("calendar") => self.calendar_page().map(|p| p.navigate_back()).unwrap_or(false),
                Some("tasks") => self.tasks_page().map(|p| p.navigate_back()).unwrap_or(false),
                Some("contacts") => self.contacts_page().map(|p| p.navigate_back()).unwrap_or(false),
                Some("memos") => self.memos_page().map(|p| p.navigate_back()).unwrap_or(false),
                _ => false,
            }
        } else {
            false
        };
        self.sync_back_button();
        handled
    }

    pub fn is_mobile(&self) -> bool {
        self.imp().is_mobile.get()
    }

    pub fn is_tablet(&self) -> bool {
        self.imp().is_tablet.get()
    }

    pub fn is_medium(&self) -> bool {
        self.imp().is_medium.get()
    }

    pub(crate) fn set_medium_screen(&self, is_medium: bool) {
        if self.imp().is_medium.get() == is_medium {
            return;
        }
        self.imp().is_medium.set(is_medium);
        if is_medium {
            self.add_css_class("medium-screen");
        } else {
            self.remove_css_class("medium-screen");
        }
        if let Some(btn) = self.imp().compose_button.borrow().as_ref() {
            let active = self.active_view_name();
            let is_compact = self.imp().is_mobile.get() || self.imp().is_tablet.get() || is_medium;
            Self::configure_primary_action(btn, &active, is_compact);
        }
        if let Some(cal) = self.calendar_page() {
            cal.set_medium_mode(is_medium);
        }
    }

    pub fn is_compact(&self) -> bool {
        self.imp().is_compact.get()
    }

    pub(crate) fn set_compact_screen(&self, is_compact: bool) {
        self.imp().is_compact.set(is_compact);
        if is_compact {
            self.add_css_class("compact-screen");
        } else {
            self.remove_css_class("compact-screen");
        }
        if let Some(cal) = self.calendar_page() {
            cal.apply_toolbar_button_modes(is_compact);
        }
        if let Some(contacts) = self.contacts_page() {
            contacts.apply_toolbar_button_modes(is_compact);
        }
        if let Some(tasks) = self.tasks_page() {
            tasks.apply_toolbar_button_modes(is_compact);
        }
        if let Some(memos) = self.memos_page() {
            memos.apply_toolbar_button_modes(is_compact);
        }
        if let Some(mail) = self.mail_page() {
            mail.apply_toolbar_button_modes(is_compact);
        }
    }

    pub(crate) fn set_screen_mode(&self, is_mobile: bool, is_tablet: bool) {
        self.imp().is_mobile.set(is_mobile);
        self.imp().is_tablet.set(is_tablet);

        if is_mobile {
            self.add_css_class("mobile-screen");
        } else {
            self.remove_css_class("mobile-screen");
            self.set_compact_screen(false);
        }

        if is_tablet {
            self.add_css_class("tablet-screen");
        } else {
            self.remove_css_class("tablet-screen");
        }

        if let Some(mail) = self.mail_page() {
            mail.set_screen_mode(is_mobile, is_tablet);
        }
        if let Some(cal) = self.calendar_page() {
            cal.set_screen_mode(is_mobile, is_tablet);
            cal.set_medium_mode(self.imp().is_medium.get());
        }
        if let Some(tasks) = self.tasks_page() {
            tasks.set_screen_mode(is_mobile, is_tablet);
        }
        if let Some(contacts) = self.contacts_page() {
            contacts.set_screen_mode(is_mobile, is_tablet);
        }
        if let Some(memos) = self.memos_page() {
            memos.set_screen_mode(is_mobile, is_tablet);
        }

        if let Some(switcher_bar) = self.imp().view_switcher_bar.borrow().as_ref() {
            switcher_bar.set_reveal(is_mobile || is_tablet);
        }

        if let Some(sb) = self.imp().statusbar_box.borrow().as_ref() {
            if is_mobile {
                sb.set_visible(false);
            } else {
                let is_vis = AppSettings::try_new()
                    .map(|s| s.mail_show_statusbar())
                    .unwrap_or(true);
                sb.set_visible(is_vis);
            }
        }

        if let Some(btn) = self.imp().compose_button.borrow().as_ref() {
            let active = self.active_view_name();
            let is_compact = is_mobile || is_tablet || self.imp().is_medium.get();
            Self::configure_primary_action(btn, &active, is_compact);
        }

        if let Some(switcher) = self.imp().view_switcher.borrow().as_ref() {
            // Keep top switcher strictly wide to prevent height jumping across responsive sizes
            switcher.set_policy(adw::ViewSwitcherPolicy::Wide);
        }

        if let Some(header_bar) = self.imp().header_bar.borrow().as_ref() {
            if is_mobile || is_tablet {
                if let Some(window_title) = self.imp().window_title.borrow().as_ref() {
                    header_bar.set_title_widget(Some(window_title));
                }
            } else {
                let is_top = AppSettings::try_new()
                    .map(|s| s.switcher_location() == "top")
                    .unwrap_or(false);
                if is_top {
                    if let Some(switcher) = self.imp().view_switcher.borrow().as_ref() {
                        header_bar.set_title_widget(Some(switcher));
                    }
                } else if let Some(window_title) = self.imp().window_title.borrow().as_ref() {
                    header_bar.set_title_widget(Some(window_title));
                }
            }
        }

        if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
            if is_mobile {
                sb_h.set_visible(false);
                sb_h.set_size_request(0, -1);
                if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                    let show_mobile_sidebar_toggle = self.active_view_name() == "mail"
                        && self.mail_page().map(|p| p.drill_down_level() == crate::views::MailDrillDownLevel::Messages).unwrap_or(false);
                    res_btn.set_visible(show_mobile_sidebar_toggle);
                }
                if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                    hb.set_show_start_title_buttons(true);
                }
            } else {
                let vis = self.is_sidebar_visible_for_active_view();
                sb_h.set_visible(vis);
                let target_pos = self.sidebar_position_for_view(&self.active_view_name());
                if vis {
                    sb_h.set_size_request(target_pos, -1);
                } else {
                    sb_h.set_size_request(0, -1);
                }
                if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                    res_btn.set_visible(!vis);
                    res_btn.set_tooltip_text(Some(&crate::i18n::gettext("Show Sidebar (F9)")));
                    set_accessible_label(res_btn, &crate::i18n::gettext("Show Sidebar"));
                }
                if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                    hb.set_show_start_title_buttons(!vis);
                }
                if vis {
                    if let Some(title) = self.imp().sidebar_title.borrow().as_ref() {
                        title.set_visible(true);
                    }
                    if let Some(btn) = self.imp().sidebar_close_button.borrow().as_ref() {
                        btn.set_visible(true);
                    }
                }
            }
        }

        self.sync_back_button();
        self.sync_right_sidebar_toggle();
    }

    pub fn sidebar_position_for_view(&self, _view_name: &str) -> i32 {
        let settings = AppSettings::try_new();
        settings
            .map(|s| s.mail_sidebar_position())
            .unwrap_or(240)
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    }

    pub fn update_sidebar_title(&self, title: &str) {
        if let Some(st) = self.imp().sidebar_title.borrow().as_ref() {
            st.set_title(title);
        }
    }

    pub fn set_sidebar_position_sync(&self, pos: i32) {
        if self.imp().is_mobile.get() {
            return;
        }
        let clamped = pos.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
        if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
            sb_h.set_size_request(clamped, -1);
        }
    }

    pub fn toggle_sidebar_animated(&self, show: bool) {
        self.imp().sidebar_visible.set(show);
        if let Some(settings) = AppSettings::try_new() {
            if settings.mail_show_sidebar() != show {
                settings.set_mail_show_sidebar(show);
            }
        }

        let is_mail = self.active_view_name() == "mail";
        if is_mail {
            if let Some(mail) = self.mail_page() {
                mail.imp().sidebar_animating.set(true);
            }
        }

        if self.imp().is_mobile.get() {
            if let Some(m) = self.mail_page() { m.set_sidebar_visible(show); }
            if let Some(c) = self.calendar_page() { c.set_sidebar_visible(show); }
            if let Some(c) = self.contacts_page() { c.set_sidebar_visible(show); }
            if let Some(t) = self.tasks_page() { t.set_sidebar_visible(show); }
            if let Some(m) = self.memos_page() { m.set_sidebar_visible(show); }
            if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                sb_h.set_visible(false);
            }
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                res_btn.set_visible(false);
            }
            if is_mail {
                if let Some(mail) = self.mail_page() {
                    mail.imp().sidebar_animating.set(false);
                }
            }
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_sidebar(show);
            }
            return;
        }

        let outer_paned = match self.active_view_outer_paned() {
            Some(p) => p,
            None => {
                if let Some(m) = self.mail_page() { m.set_sidebar_visible(show); }
                if let Some(c) = self.calendar_page() { c.set_sidebar_visible(show); }
                if let Some(c) = self.contacts_page() { c.set_sidebar_visible(show); }
                if let Some(t) = self.tasks_page() { t.set_sidebar_visible(show); }
                if let Some(m) = self.memos_page() { m.set_sidebar_visible(show); }
                if is_mail {
                    if let Some(mail) = self.mail_page() {
                        mail.imp().sidebar_animating.set(false);
                    }
                }
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_show_sidebar(show);
                }
                return;
            }
        };

        let target_pos = self.sidebar_position_for_view(&self.active_view_name());

        let is_start_visible = outer_paned.start_child().map(|s| s.property::<bool>("visible")).unwrap_or(false);
        let current_pos = outer_paned.position();
        let (from_pos, to_pos) = if show {
            let start = if !is_start_visible || current_pos <= 10 {
                0.0
            } else {
                current_pos as f64
            };
            (start, target_pos as f64)
        } else {
            let start = if current_pos < 180 {
                target_pos as f64
            } else {
                current_pos as f64
            };
            (start, 0.0)
        };

        if let Some(old_anim) = self.imp().sidebar_animation.borrow_mut().take() {
            old_anim.pause();
        }

        if show {
            if let Some(start) = outer_paned.start_child() {
                start.set_visible(true);
            }
            if !self.imp().is_mobile.get() {
                if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                    sb_h.set_visible(true);
                    sb_h.set_show_start_title_buttons(true);
                    sb_h.set_size_request(from_pos as i32, -1);
                }
                if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                    hb.set_show_start_title_buttons(false);
                }
            } else {
                if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                    sb_h.set_visible(false);
                    sb_h.set_size_request(0, -1);
                }
                if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                    hb.set_show_start_title_buttons(true);
                }
            }
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                res_btn.set_visible(false);
            }
            if from_pos < 170.0 {
                if let Some(title) = self.imp().sidebar_title.borrow().as_ref() {
                    title.set_visible(false);
                }
                if let Some(btn) = self.imp().sidebar_close_button.borrow().as_ref() {
                    btn.set_visible(false);
                }
            } else {
                if let Some(title) = self.imp().sidebar_title.borrow().as_ref() {
                    title.set_visible(true);
                }
                if let Some(btn) = self.imp().sidebar_close_button.borrow().as_ref() {
                    btn.set_visible(true);
                }
            }
        } else {
            if let Some(title) = self.imp().sidebar_title.borrow().as_ref() {
                title.set_visible(false);
            }
            if let Some(btn) = self.imp().sidebar_close_button.borrow().as_ref() {
                btn.set_visible(false);
            }
            if let Some(sb_h) = self.imp().sidebar_header.borrow().as_ref() {
                sb_h.set_visible(false);
                sb_h.set_size_request(0, -1);
                sb_h.set_show_start_title_buttons(false);
            }
            if let Some(res_btn) = self.imp().restore_sidebar_button.borrow().as_ref() {
                res_btn.set_visible(true);
                res_btn.set_tooltip_text(Some(&crate::i18n::gettext("Show Sidebar (F9)")));
                set_accessible_label(res_btn, &crate::i18n::gettext("Show Sidebar"));
            }
            if let Some(hb) = self.imp().header_bar.borrow().as_ref() {
                hb.set_show_start_title_buttons(true);
            }
        }

        let win_weak = self.downgrade();
        let paned_weak = outer_paned.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            let pos = value.round() as i32;
            if let Some(paned) = paned_weak.upgrade() {
                paned.set_position(pos);
            }
            if let Some(win) = win_weak.upgrade() {
                if !win.imp().is_mobile.get() {
                    if let Some(sb_h) = win.imp().sidebar_header.borrow().as_ref() {
                        sb_h.set_size_request(pos, -1);
                    }
                }
                if show && pos >= 170 {
                    if let Some(title) = win.imp().sidebar_title.borrow().as_ref() {
                        if !title.is_visible() {
                            title.set_visible(true);
                        }
                    }
                    if let Some(btn) = win.imp().sidebar_close_button.borrow().as_ref() {
                        if !btn.is_visible() {
                            btn.set_visible(true);
                        }
                    }
                }
            }
        });

        let duration = if show { 160 } else { 140 };
        let animation = adw::TimedAnimation::builder()
            .widget(&*self)
            .value_from(from_pos)
            .value_to(to_pos)
            .duration(duration)
            .easing(adw::Easing::EaseOutCubic)
            .target(&target)
            .build();

        let win_weak_done = self.downgrade();
        let paned_weak_done = outer_paned.downgrade();
        animation.connect_done(move |_| {
            if let Some(paned) = paned_weak_done.upgrade() {
                if !show {
                    if let Some(start) = paned.start_child() {
                        start.set_visible(false);
                    }
                    paned.set_position(0);
                } else {
                    paned.set_position(target_pos);
                }
            }
            if let Some(win) = win_weak_done.upgrade() {
                win.imp().sidebar_animation.borrow_mut().take();
                if is_mail {
                    if let Some(mail) = win.mail_page() {
                        mail.imp().sidebar_animating.set(false);
                    }
                }
                if let Some(m) = win.mail_page() { m.set_sidebar_visible(show); }
                if let Some(c) = win.calendar_page() { c.set_sidebar_visible(show); }
                if let Some(c) = win.contacts_page() { c.set_sidebar_visible(show); }
                if let Some(t) = win.tasks_page() { t.set_sidebar_visible(show); }
                if let Some(m) = win.memos_page() { m.set_sidebar_visible(show); }

                if !show {
                    if let Some(sb_h) = win.imp().sidebar_header.borrow().as_ref() {
                        sb_h.set_visible(false);
                        sb_h.set_size_request(0, -1);
                        sb_h.set_show_start_title_buttons(true);
                    }
                    if let Some(res_btn) = win.imp().restore_sidebar_button.borrow().as_ref() {
                        res_btn.set_visible(true);
                        res_btn.set_tooltip_text(Some(&crate::i18n::gettext("Show Sidebar (F9)")));
                        set_accessible_label(res_btn, &crate::i18n::gettext("Show Sidebar"));
                    }
                    if let Some(hb) = win.imp().header_bar.borrow().as_ref() {
                        hb.set_show_start_title_buttons(true);
                    }
                    if let Some(title) = win.imp().sidebar_title.borrow().as_ref() {
                        title.set_visible(true);
                    }
                    if let Some(btn) = win.imp().sidebar_close_button.borrow().as_ref() {
                        btn.set_visible(true);
                    }
                } else {
                    if !win.imp().is_mobile.get() {
                        if let Some(sb_h) = win.imp().sidebar_header.borrow().as_ref() {
                            sb_h.set_visible(true);
                            sb_h.set_size_request(target_pos, -1);
                        }
                    } else {
                        if let Some(sb_h) = win.imp().sidebar_header.borrow().as_ref() {
                            sb_h.set_visible(false);
                            sb_h.set_size_request(0, -1);
                        }
                    }
                    if let Some(res_btn) = win.imp().restore_sidebar_button.borrow().as_ref() {
                        res_btn.set_visible(false);
                    }
                    if let Some(title) = win.imp().sidebar_title.borrow().as_ref() {
                        title.set_visible(true);
                    }
                    if let Some(btn) = win.imp().sidebar_close_button.borrow().as_ref() {
                        btn.set_visible(true);
                    }
                }
                if let Some(action) = win.lookup_action("toggle-sidebar").and_then(|a| a.downcast::<gio::SimpleAction>().ok()) {
                    if action.state().and_then(|v| v.get::<bool>()) != Some(show) {
                        action.set_state(&show.to_variant());
                    }
                }
                if let Some(settings) = AppSettings::try_new() {
                    if settings.mail_show_sidebar() != show {
                        settings.set_mail_show_sidebar(show);
                    }
                    if show && (settings.mail_sidebar_position() < MIN_SIDEBAR_WIDTH || settings.mail_sidebar_position() > MAX_SIDEBAR_WIDTH) {
                        settings.set_mail_sidebar_position(target_pos);
                    }
                }
            }
            let win_for_idle = win_weak_done.clone();
            glib::idle_add_local_once(move || {
                if let Some(win) = win_for_idle.upgrade() {
                    if show {
                        if let Some(paned) = win.active_view_outer_paned() {
                            paned.set_position(target_pos);
                        }
                    }
                    if is_mail {
                        if let Some(mail) = win.mail_page() {
                            mail.imp().sidebar_animating.set(false);
                        }
                    }
                }
            });
        });

        self.imp().sidebar_animation.replace(Some(animation.clone()));
        animation.play();
    }

    pub(crate) fn present_new_task(&self) {
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

    pub(crate) fn present_new_memo(&self) {
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
        if self.is_mobile() || self.is_tablet() {
            dialog.present_modal(self);
        } else {
            dialog.present_standalone();
        }
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

    pub(crate) fn reorder_account_roots(&self) {
        if let Some(mail_page) = self.mail_page() {
            mail_page.reorder_account_roots();
        }
    }

    pub(crate) fn reload_accounts(&self) {
        crate::services::eds_service::invalidate_account_collections_cache();
        crate::services::camel_mail_service::reset_global_session();
        crate::services::camel_mail_service::clear_mail_load_cache();
        if let Some(mail_page) = self.mail_page() {
            mail_page.reload_mail_accounts();
        }
        if let Some(contacts_page) = self.contacts_page() {
            contacts_page.reload_address_books();
        }
        if let Some(calendar_page) = self.calendar_page() {
            calendar_page.reload_calendar_sources();
        }
        if let Some(tasks_page) = self.tasks_page() {
            tasks_page.reload_task_sources();
        }
        if let Some(memos_page) = self.memos_page() {
            memos_page.reload_memo_sources();
        }
    }

    pub(crate) fn setup_account_monitoring(&self) {
        let window = self.clone();
        let trigger_reload = move || {
            let window = window.clone();
            if let Some(source) = window.imp().account_reload_source.borrow_mut().take() {
                source.remove();
            }
            let w_timeout = window.clone();
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(150),
                move || {
                    w_timeout.imp().account_reload_source.borrow_mut().take();
                    tracing::info!("Account configuration or EDS source changed, reloading accounts across all views");
                    w_timeout.reload_accounts();
                },
            );
            window.imp().account_reload_source.replace(Some(source_id));
        };

        // 1. File monitoring on Evolution sources and GOA configuration directories
        let mut source_dirs = Vec::new();
        let config_sources = glib::user_config_dir().join("evolution/sources");
        source_dirs.push(config_sources);

        let goa_dir = glib::user_config_dir().join("goa-1.0");
        if goa_dir.exists() {
            source_dirs.push(goa_dir);
        }

        let home = glib::home_dir();
        let flatpak_sources = home.join(".var/app/org.gnome.Evolution/config/evolution/sources");
        if flatpak_sources.exists() {
            source_dirs.push(flatpak_sources);
        }

        let mut monitors = Vec::new();
        for dir in source_dirs {
            let file = gio::File::for_path(&dir);
            match file.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
                Ok(monitor) => {
                    let trigger = trigger_reload.clone();
                    monitor.connect_changed(move |_mon, _file, _other_file, _event| {
                        trigger();
                    });
                    monitors.push(monitor);
                    tracing::debug!(path = ?dir, "Monitoring account sources directory for changes");
                }
                Err(err) => {
                    tracing::debug!(path = ?dir, ?err, "Could not monitor account sources directory");
                }
            }
        }
        self.imp().account_file_monitors.replace(monitors);

        // 2. D-Bus signal subscription for GOA and EDS
        let window_weak = self.downgrade();
        match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(bus) => {
                let trigger_for_dbus = {
                    let window_weak = window_weak.clone();
                    move || {
                        if let Some(w) = window_weak.upgrade() {
                            if let Some(source) = w.imp().account_reload_source.borrow_mut().take() {
                                source.remove();
                            }
                            let w_timeout = w.clone();
                            let source_id = glib::timeout_add_local_once(
                                std::time::Duration::from_millis(150),
                                move || {
                                    w_timeout.imp().account_reload_source.borrow_mut().take();
                                    tracing::info!("D-Bus account change detected, reloading accounts");
                                    w_timeout.reload_accounts();
                                },
                            );
                            w.imp().account_reload_source.replace(Some(source_id));
                        }
                    }
                };

                let mut subs = Vec::new();

                // GOA ObjectManager (InterfacesAdded / InterfacesRemoved)
                let trigger1 = trigger_for_dbus.clone();
                subs.push(bus.signal_subscribe(
                    Some("org.gnome.OnlineAccounts"),
                    Some("org.freedesktop.DBus.ObjectManager"),
                    None,
                    Some("/org/gnome/OnlineAccounts"),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_conn, _sender, _path, _iface, _signal, _params| {
                        trigger1();
                    },
                ));

                // GOA PropertiesChanged (e.g. MailDisabled / CalendarDisabled toggles)
                let trigger2 = trigger_for_dbus.clone();
                subs.push(bus.signal_subscribe(
                    Some("org.gnome.OnlineAccounts"),
                    Some("org.freedesktop.DBus.Properties"),
                    Some("PropertiesChanged"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_conn, _sender, _path, _iface, _signal, _params| {
                        trigger2();
                    },
                ));

                // EDS SourceManager ObjectManager
                let trigger3 = trigger_for_dbus.clone();
                subs.push(bus.signal_subscribe(
                    Some("org.gnome.evolution.dataserver.Sources5"),
                    Some("org.freedesktop.DBus.ObjectManager"),
                    None,
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_conn, _sender, _path, _iface, _signal, _params| {
                        trigger3();
                    },
                ));

                // EDS SourceManager PropertiesChanged (e.g. Enabled / MailEnabled toggles)
                let trigger4 = trigger_for_dbus.clone();
                subs.push(bus.signal_subscribe(
                    Some("org.gnome.evolution.dataserver.Sources5"),
                    Some("org.freedesktop.DBus.Properties"),
                    Some("PropertiesChanged"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_conn, _sender, _path, _iface, _signal, _params| {
                        trigger4();
                    },
                ));

                self.imp().account_bus_subscriptions.replace(subs);
                tracing::debug!("D-Bus subscriptions for GOA and EDS source changes established");
            }
            Err(err) => {
                tracing::warn!(?err, "Could not connect to session D-Bus for account change monitoring");
            }
        }
    }

    pub(crate) fn reload_mail_accounts(&self) {
        self.reload_accounts();
    }


    pub(crate) fn update_mail_status(&self, status_text: &str) {
        if let Some(label) = self.imp().status_label.borrow().as_ref() {
            let status_text = status_text.trim();
            if status_text.is_empty() {
                let offline = AppSettings::try_new()
                    .map(|s| s.offline_mode())
                    .unwrap_or(false);
                let offline_str = crate::i18n::gettext("Offline");
                let ready_str = crate::i18n::gettext("Ready");
                let default_status = if offline {
                    format!("Meridian  ·  {offline_str}")
                } else {
                    format!("Meridian  ·  {ready_str}")
                };
                label.set_label(&default_status);
            } else {
                let status = format!("Meridian  ·  {status_text}");
                label.set_label(&status);
            }
        }
    }

    /// Public helper for any view/component to publish a status update to the window statusbar.
    pub fn set_status(&self, status_text: &str) {
        self.update_mail_status(status_text);
    }

}

pub type EvolutionWindow = MeridianWindow;

/// Backward compatibility shim for finding the active window.
#[inline]
pub fn find_evolution_window(widget: &impl gtk::prelude::IsA<gtk::Widget>) -> Option<MeridianWindow> {
    find_meridian_window(widget)
}

/// Find the active MeridianWindow from any widget or dialog.
pub fn find_meridian_window(widget: &impl gtk::prelude::IsA<gtk::Widget>) -> Option<MeridianWindow> {
    if let Ok(win) = widget.clone().upcast::<gtk::Widget>().downcast::<MeridianWindow>() {
        return Some(win);
    }
    if let Some(root) = widget.root() {
        if let Ok(win) = root.downcast::<MeridianWindow>() {
            return Some(win);
        }
    }
    let mut current_parent = widget.clone().upcast::<gtk::Widget>().parent();
    while let Some(parent) = current_parent {
        if let Ok(meridian_win) = parent.clone().downcast::<MeridianWindow>() {
            return Some(meridian_win);
        }
        current_parent = parent.parent();
    }
    let mut current_window = widget
        .clone()
        .upcast::<gtk::Widget>()
        .downcast::<gtk::Window>()
        .ok()
        .or_else(|| widget.root().and_then(|r| r.downcast::<gtk::Window>().ok()));
    while let Some(win) = current_window {
        if let Ok(meridian_win) = win.clone().downcast::<MeridianWindow>() {
            return Some(meridian_win);
        }
        current_window = win.transient_for();
    }
    let toplevels = gtk::Window::toplevels();
    for i in 0..toplevels.n_items() {
        if let Some(toplevel) = toplevels.item(i) {
            if let Ok(meridian_win) = toplevel.downcast::<MeridianWindow>() {
                return Some(meridian_win);
            }
        }
    }
    None
}

impl MeridianWindow {

    /// Toggle the developer protocol console drawer.
    pub fn toggle_debug_console(&self) {
        if let Some(revealer) = self.imp().debug_revealer.borrow().as_ref() {
            let new_state = !revealer.reveals_child();
            if new_state {
                revealer.set_visible(true);
            }
            revealer.set_reveal_child(new_state);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_debug_console_enabled(new_state);
            }
        }
    }

    /// Explicitly open or close the developer protocol console drawer.
    pub fn set_debug_console_open(&self, open: bool) {
        if let Some(revealer) = self.imp().debug_revealer.borrow().as_ref() {
            if open {
                revealer.set_visible(true);
            }
            revealer.set_reveal_child(open);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_debug_console_enabled(open);
            }
        }
    }

    /// Explicitly set the visibility of the main window statusbar.
    pub fn set_statusbar_visible(&self, visible: bool) {
        if let Some(sb) = self.imp().statusbar_box.borrow().as_ref() {
            sb.set_visible(visible);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_mail_show_statusbar(visible);
        }
    }

    /// Check if the main window statusbar is currently visible.
    pub fn statusbar_is_visible(&self) -> bool {
        self.imp()
            .statusbar_box
            .borrow()
            .as_ref()
            .map(|sb| sb.is_visible())
            .unwrap_or_else(|| {
                AppSettings::try_new()
                    .map(|s| s.mail_show_statusbar())
                    .unwrap_or(true)
            })
    }

    fn update_transport_action_sensitivity(&self, snapshot: &MailTransportSnapshot) {
        for (name, enabled) in [
            ("mail-send-receive", snapshot.actions.mail_send_receive),
            ("mail-receive-all", snapshot.actions.mail_receive_all),
            ("mail-send-all", snapshot.actions.mail_send_all),
            (MAIL_STOP_ACTION_ID, snapshot.actions.mail_stop),
            (WORK_OFFLINE_ACTION_ID, snapshot.actions.work_offline),
            (WORK_ONLINE_ACTION_ID, snapshot.actions.work_online),
        ] {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
    }

    fn update_transport_presentation(&self, snapshot: &MailTransportSnapshot) {
        let current_generation = self.imp().transport_generation.get();
        if snapshot.generation < current_generation {
            return;
        }
        self.imp().transport_generation.set(snapshot.generation);
        self.imp().transport_queued_items.set(snapshot.queued_items);

        if let Some(toggle) = self.imp().online_toggle.borrow().as_ref() {
            let offline = snapshot.mode == MailTransportMode::Offline;
            toggle.set_icon_name(if offline {
                "network-offline-symbolic"
            } else {
                "network-idle-symbolic"
            });
            let tooltip = if offline {
                crate::i18n::gettext("Offline mode active; click to switch online")
            } else {
                crate::i18n::gettext("Online mode active; click to switch offline")
            };
            toggle.set_tooltip_text(Some(&tooltip));
        }

        if let Some(label) = self.imp().status_label.borrow().as_ref() {
            label.set_label(&format!("Meridian  ·  {}", snapshot.status_text()));
        }

        if let Some(progress) = self.imp().status_progress.borrow().as_ref() {
            let active = matches!(
                snapshot.state,
                MailTransportState::Running
                    | MailTransportState::Retrying
                    | MailTransportState::CancelRequested
            );
            progress.set_visible(active);
            if let Some(total) = snapshot.progress.total.filter(|total| *total > 0) {
                progress.set_fraction(
                    (snapshot.progress.completed as f64 / total as f64).clamp(0.0, 1.0),
                );
            } else if active {
                progress.pulse();
            } else {
                progress.set_fraction(0.0);
            }
        }

        self.update_transport_action_sensitivity(snapshot);
    }

    fn sync_transport_mode(&self, offline: bool, refresh_mail: bool) {
        let mode = if offline {
            MailTransportMode::Offline
        } else {
            MailTransportMode::Online
        };
        let snapshot = self.imp().transport_service.set_mode(mode);
        self.update_transport_presentation(&snapshot);
        if refresh_mail {
            // MailPage's existing generation/cancellation gate is the owner
            // of its model load.  Refreshing after the setting change makes
            // that gate stop remote work and load the local snapshot offline,
            // or restart the provider path online.
            self.refresh_mail_page();
        }
        if !offline {
            self.drain_transport_queue();
        }
    }

    fn set_offline_mode(&self, offline: bool) {
        if let Some(settings) = AppSettings::try_new() {
            settings.set_offline_mode(offline);
        } else {
            self.sync_transport_mode(offline, true);
        }
    }

    fn start_mail_transport(&self, action: MailTransportAction) {
        self.start_mail_transport_with_feedback(action, true);
    }

    fn start_mail_transport_with_feedback(
        &self,
        action: MailTransportAction,
        show_start_error: bool,
    ) {
        let operation = match self.imp().transport_service.start(action) {
            Ok(operation) => operation,
            Err(error) => {
                self.update_transport_presentation(&self.imp().transport_service.snapshot());
                if show_start_error {
                    let message = match error {
                        MailTransportStartError::AlreadyRunning => {
                            gettext("A mail transfer is already in progress")
                        }
                        MailTransportStartError::Offline => {
                            gettext("Offline mode: message remains in the local outgoing queue")
                        }
                        MailTransportStartError::NoQueuedMessages => {
                            gettext("No pending messages")
                        }
                    };
                    self.add_toast(&adw::Toast::new(&message));
                }
                return;
            }
        };

        self.update_transport_presentation(&self.imp().transport_service.snapshot());
        self.watch_mail_transport(operation, action);
    }

    pub(crate) fn drain_transport_queue(&self) {
        let Some(operation) = self.imp().transport_service.start_queued_if_online() else {
            return;
        };
        self.update_transport_presentation(&self.imp().transport_service.snapshot());
        self.watch_mail_transport(operation, MailTransportAction::SendAll);
    }

    /// Execute configured exit maintenance tasks: junk purge and folder expunge.
    pub(crate) fn execute_exit_purge_and_expunge(&self) {
        let settings = AppSettings::try_new();
        let should_empty_junk = settings.as_ref().is_some_and(|s| s.junk_empty_on_exit());
        let junk_days = settings.as_ref().map(|s| s.junk_empty_on_exit_days()).unwrap_or(0);

        let accounts = crate::services::account_config_service::AccountConfigStore::from_environment().load();
        let should_expunge_trash = accounts.iter().any(|p| p.enabled && p.expunge_on_exit);

        if should_empty_junk || should_expunge_trash {
            let store = crate::services::mail_store_service::MailStoreService::new();
            if should_empty_junk {
                match store.empty_local_junk_with_retention(junk_days) {
                    Ok(count) => tracing::info!("Exit junk purge completed: {count} messages purged"),
                    Err(err) => tracing::warn!("Exit junk purge failed: {err}"),
                }
            }
            if should_expunge_trash {
                match store.empty_local_trash() {
                    Ok(count) => tracing::info!("Exit folder expunge completed: {count} messages purged"),
                    Err(err) => tracing::warn!("Exit folder expunge failed: {err}"),
                }
            }
        }
    }

    fn watch_mail_transport(&self, operation: MailTransportOperation, action: MailTransportAction) {
        let window = self.downgrade();
        let generation = operation.generation;
        glib::spawn_future_local(async move {
            while let Ok(event) = operation.events.recv().await {
                let Some(window) = window.upgrade() else {
                    break;
                };
                if event.generation != window.imp().transport_generation.get() {
                    // The setting changed or a newer operation won.  The
                    // service still completes its provider work, but this
                    // result is intentionally invisible to GTK.
                    continue;
                }
                let snapshot = event.snapshot;
                let terminal = !matches!(
                    snapshot.state,
                    MailTransportState::Running
                        | MailTransportState::Retrying
                        | MailTransportState::CancelRequested
                ) && snapshot.action.is_none();
                window.update_transport_presentation(&snapshot);
                if terminal
                    && snapshot.state == MailTransportState::Failed
                    && let Some(error) = snapshot.error.as_ref()
                {
                    window.add_toast(&adw::Toast::new(&format!("Mail-Transport: {error}")));
                }
                if terminal
                    && snapshot.state == MailTransportState::Succeeded
                    && matches!(
                        action,
                        MailTransportAction::SendReceive | MailTransportAction::ReceiveAll
                    )
                {
                    // The transport contract intentionally returns no GTK
                    // model.  Reuse MailPage's own bounded worker/generation
                    // path to apply the fresh provider snapshot.
                    window.refresh_mail_page();
                }
                if terminal
                    && snapshot.mode == MailTransportMode::Online
                    && snapshot.queued_items > 0
                    && snapshot.message.as_deref() == Some("Veraltetes Transportergebnis verworfen")
                {
                    // A mode change invalidated the old worker.  Once its
                    // provider call has actually unwound, drain only the
                    // messages still lacking provider confirmation.
                    window.drain_transport_queue();
                }
            }
            tracing::debug!(generation, "Mail transport event stream closed");
        });
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

    pub(crate) fn show_search_result(&self, target: Option<SearchTarget>) {
        let route = match search_route_for_target(target.as_ref()) {
            Ok(route) => route,
            Err(error) => {
                self.add_toast(&adw::Toast::new(&search_route_error_message(error)));
                return;
            }
        };

        let generation = self.begin_search_route(route);
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name(route.page_name());
        }

        let window = self.downgrade();
        let mut attempts = 0u8;
        let source = glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if window.search_route_decision(generation, route, attempts, false)
                == SearchRouteDecision::RejectStale
            {
                window.invalidate_search_route_if_current(generation);
                return glib::ControlFlow::Break;
            }

            let found = target
                .as_ref()
                .is_some_and(|target| window.open_search_target(route, target));
            attempts = attempts.saturating_add(1);
            match window.search_route_decision(generation, route, attempts, found) {
                SearchRouteDecision::Complete => {
                    window.finish_search_route(generation);
                    glib::ControlFlow::Break
                }
                SearchRouteDecision::Retry => glib::ControlFlow::Continue,
                SearchRouteDecision::Failed => {
                    window.finish_search_route(generation);
                    let msg = gettext("Search match could not be opened after waiting");
                    window.add_toast(&adw::Toast::new(&msg));
                    glib::ControlFlow::Break
                }
                SearchRouteDecision::RejectStale => {
                    window.invalidate_search_route_if_current(generation);
                    glib::ControlFlow::Break
                }
            }
        });
        self.imp().search_route_source.borrow_mut().replace(source);
        let msg = match route {
            SearchRoute::Mail => gettext("Mail view opened"),
            SearchRoute::Calendar => gettext("Calendar view opened; searching for match"),
            SearchRoute::Tasks => gettext("Tasks view opened; searching for match"),
            SearchRoute::Memos => gettext("Memos view opened; searching for match"),
            SearchRoute::Contacts => gettext("Contacts view opened; searching for match"),
        };
        self.add_toast(&adw::Toast::new(&msg));
    }

    fn begin_search_route(&self, route: SearchRoute) -> u64 {
        self.cancel_search_route_timer();
        let mut state = self.imp().search_route_state.get();
        let generation = state.begin(route);
        self.imp().search_route_state.set(state);
        generation
    }

    fn cancel_search_route_timer(&self) {
        if let Some(source) = self.imp().search_route_source.borrow_mut().take() {
            source.remove();
        }
    }

    fn invalidate_search_route_for_visible_page(&self, visible_page: Option<&str>) {
        let mut state = self.imp().search_route_state.get();
        if state.invalidate_for_visible_page(visible_page) {
            self.cancel_search_route_timer();
            self.imp().search_route_state.set(state);
        }
    }

    fn invalidate_search_route_if_current(&self, generation: u64) {
        let mut state = self.imp().search_route_state.get();
        if state.generation == generation && state.destination.is_some() {
            state.generation = next_search_route_generation(state.generation);
            state.destination = None;
            self.cancel_search_route_timer();
            self.imp().search_route_state.set(state);
        }
    }

    fn finish_search_route(&self, generation: u64) {
        let mut state = self.imp().search_route_state.get();
        if state.generation == generation {
            state.complete(generation);
            self.imp().search_route_source.borrow_mut().take();
            self.imp().search_route_state.set(state);
        }
    }

    fn search_route_decision(
        &self,
        generation: u64,
        route: SearchRoute,
        attempts: u8,
        found: bool,
    ) -> SearchRouteDecision {
        let visible_page = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .map(|name| name.to_string());
        search_route_decision(
            self.imp().search_route_state.get(),
            generation,
            route,
            visible_page.as_deref(),
            attempts,
            found,
        )
    }

    fn open_search_target(&self, route: SearchRoute, target: &SearchTarget) -> bool {
        match (route, target) {
            (SearchRoute::Mail, SearchTarget::Mail { local_id }) => self
                .mail_page()
                .is_some_and(|page| page.select_search_result(local_id)),
            (
                SearchRoute::Calendar,
                SearchTarget::Event {
                    source_uid,
                    component_uid,
                },
            ) => self
                .calendar_page()
                .is_some_and(|page| page.open_search_result(source_uid, component_uid)),
            (
                SearchRoute::Tasks,
                SearchTarget::Task {
                    source_uid,
                    component_uid,
                },
            ) => self
                .tasks_page()
                .is_some_and(|page| page.open_search_result(source_uid, component_uid)),
            (
                SearchRoute::Memos,
                SearchTarget::Memo {
                    source_uid,
                    component_uid,
                },
            ) => self
                .memos_page()
                .is_some_and(|page| page.select_search_result(source_uid, component_uid)),
            (
                SearchRoute::Contacts,
                SearchTarget::Contact {
                    source_uid,
                    contact_uid,
                },
            ) => self
                .contacts_page()
                .is_some_and(|page| page.select_search_result(source_uid, contact_uid)),
            _ => false,
        }
    }

    fn focus_active_search(&self) {
        if let Some(btn) = self.imp().search_button.borrow().as_ref() {
            if !btn.is_active() {
                btn.set_active(true);
            }
        }
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

    fn close_active_search(&self) {
        if let Some(btn) = self.imp().search_button.borrow().as_ref() {
            if btn.is_active() {
                btn.set_active(false);
            }
        }
        let view_name = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .map(|name| name.to_string());
        match view_name.as_deref() {
            Some("calendar") => {
                if let Some(page) = self.calendar_page() {
                    page.close_search();
                }
            }
            Some("contacts") => {
                if let Some(page) = self.contacts_page() {
                    page.close_search();
                }
            }
            Some("tasks") => {
                if let Some(page) = self.tasks_page() {
                    page.close_search();
                }
            }
            Some("memos") => {
                if let Some(page) = self.memos_page() {
                    page.close_search();
                }
            }
            _ => {
                if let Some(page) = self.mail_page() {
                    page.close_search();
                }
            }
        };
    }

    pub fn toggle_active_search(&self) {
        if self.is_active_search_open() {
            self.close_active_search();
        } else {
            self.focus_active_search();
        }
    }

    fn is_active_search_open(&self) -> bool {
        let view_name = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .map(|name| name.to_string());
        match view_name.as_deref() {
            Some("calendar") => self.calendar_page().map(|p| p.is_search_active()).unwrap_or(false),
            Some("contacts") => self.contacts_page().map(|p| p.is_search_active()).unwrap_or(false),
            Some("tasks") => self.tasks_page().map(|p| p.is_search_active()).unwrap_or(false),
            Some("memos") => self.memos_page().map(|p| p.is_search_active()).unwrap_or(false),
            _ => self.mail_page().map(|p| p.is_search_active()).unwrap_or(false),
        }
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
        let read_rc = std::rc::Rc::new(read);
        let initial = settings.as_ref().map(|s| read_rc(s)).unwrap_or(false);
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let settings_for_change = settings.clone();
        let write_rc = std::rc::Rc::new(write);
        let write_for_change = write_rc.clone();
        let write_for_activate = write_rc.clone();
        action.connect_change_state(move |action, new_state| {
            if let Some(state) = new_state {
                action.set_state(state);
                if let Some(val) = state.get::<bool>() {
                    if let Some(settings) = settings_for_change.as_ref() {
                        write_for_change(settings, val);
                    }
                }
            }
        });

        let action_for_activate = action.clone();
        let settings_for_activate = settings.clone();
        action.connect_activate(move |_, _| {
            let current = action_for_activate
                .state()
                .and_then(|state| state.get::<bool>())
                .unwrap_or(false);
            let next = !current;
            action_for_activate.set_state(&next.to_variant());
            if let Some(settings) = settings_for_activate.as_ref() {
                write_for_activate(settings, next);
            }
        });

        if let Some(app_settings) = settings {
            let action_for_settings = action.clone();
            let read_for_settings = read_rc.clone();
            let app_settings_for_signal = app_settings.clone();
            app_settings.connect_changed(Some(key), move |_, _| {
                action_for_settings.set_state(&read_for_settings(&app_settings_for_signal).to_variant());
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
        let read_rc = std::rc::Rc::new(read);
        let initial = settings
            .as_ref()
            .is_some_and(|settings| read_rc(settings) == value);
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());

        let value_for_change = value.to_string();
        let settings_for_change = settings.clone();
        let write_rc = std::rc::Rc::new(write);
        let write_for_change = write_rc.clone();
        let write_for_activate = write_rc.clone();

        action.connect_change_state(move |action, new_state| {
            if let Some(state) = new_state {
                if state.get::<bool>() == Some(true) {
                    action.set_state(&true.to_variant());
                    if let Some(settings) = settings_for_change.as_ref() {
                        write_for_change(settings, &value_for_change);
                    }
                }
            }
        });

        let value_for_activate = value.to_string();
        let settings_for_activate = settings.clone();
        action.connect_activate(move |action, _| {
            if let Some(settings) = settings_for_activate.as_ref() {
                write_for_activate(settings, &value_for_activate);
            }
            action.set_state(&true.to_variant());
        });

        if let Some(app_settings) = settings {
            let action_for_settings = action.clone();
            let read_for_settings = read_rc.clone();
            let value_for_settings = value.to_string();
            let app_settings_for_signal = app_settings.clone();
            app_settings.connect_changed(Some(key), move |_, _| {
                action_for_settings
                    .set_state(&(read_for_settings(&app_settings_for_signal) == value_for_settings).to_variant());
            });
        }
        self.add_action(&action);
        action
    }

    fn configure_primary_action(button: &gtk::Button, view_name: &str, is_compact: bool) {
        let (icon_name, label_text, tooltip, action_name) = match view_name {
            "calendar" => (
                "x-office-calendar-symbolic",
                crate::i18n::gettext("New Event"),
                crate::i18n::gettext("Create new event (Ctrl+N)"),
                "win.new",
            ),
            "contacts" => (
                "contact-new-symbolic",
                crate::i18n::gettext("New Contact"),
                crate::i18n::gettext("Create new contact (Ctrl+N)"),
                "win.new",
            ),
            "tasks" => (
                "task-new-symbolic",
                crate::i18n::gettext("New Task"),
                crate::i18n::gettext("Create new task (Ctrl+N)"),
                "win.new",
            ),
            "memos" => (
                "document-new-symbolic",
                crate::i18n::gettext("New Memo"),
                crate::i18n::gettext("Create new note (Ctrl+N)"),
                "win.new",
            ),
            _ => (
                "mail-message-new-symbolic",
                crate::i18n::gettext("New Mail"),
                crate::i18n::gettext("Create new email (Ctrl+N)"),
                "win.new",
            ),
        };
        let icon_theme = gtk::IconTheme::for_display(&button.display());
        let icon_actual = if icon_theme.has_icon("list-add-symbolic") {
            "list-add-symbolic"
        } else if icon_theme.has_icon(icon_name) {
            icon_name
        } else {
            "list-add-symbolic"
        };
        if is_compact {
            let img = gtk::Image::from_icon_name(icon_actual);
            button.set_child(Some(&img));
        } else {
            let label = gtk::Label::builder()
                .label(&label_text)
                .valign(gtk::Align::Center)
                .build();
            button.set_child(Some(&label));
        }
        button.set_tooltip_text(Some(&tooltip));
        button.set_action_name(Some(action_name));
        button.add_css_class("suggested-action");
        set_accessible_label(button, &label_text);
    }

    fn set_mail_actions_enabled(&self, enabled: bool) {
        for spec in ACTIONS.iter().filter(|spec| {
            matches!(
                spec.scope,
                ActionScope::ActiveMail
                    | ActionScope::MailList
                    | ActionScope::MessageSelection
                    | ActionScope::FolderSelection
            )
        }) {
            let name = spec.action.trim_start_matches("win.");
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
        self.set_message_actions_enabled(enabled);
        self.update_mail_undelete_action(enabled);
        self.update_folder_properties_action(enabled);
        self.update_folder_subscriptions_action(enabled);
        self.update_folder_sort_order_action(enabled);
        self.update_mail_navigation_actions(enabled);
    }

    fn set_calendar_actions_enabled(&self, enabled: bool) {
        if let Some(action) = self
            .lookup_action("purge-calendar")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
        if let Some(action) = self
            .lookup_action("export-calendar")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    fn update_folder_sort_order_action(&self, mail_view_enabled: bool) {
        let enabled = mail_view_enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.can_edit_folder_sort_order());
        if let Some(action) = self
            .lookup_action("folder-edit-sort-order")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    fn update_folder_properties_action(&self, mail_view_enabled: bool) {
        let enabled = mail_view_enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.can_present_folder_properties());
        self.set_folder_properties_action_enabled(enabled);
    }

    pub(crate) fn set_folder_properties_action_enabled(&self, enabled: bool) {
        if let Some(action) = self
            .lookup_action("folder-properties")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    pub(crate) fn update_folder_properties_action_for_current_view(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.update_folder_properties_action(is_mail_view);
    }

    fn update_folder_subscriptions_action(&self, mail_view_enabled: bool) {
        let enabled = mail_view_enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.can_present_folder_subscriptions());
        if let Some(action) = self
            .lookup_action("folder-subscriptions")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    pub(crate) fn update_folder_subscriptions_action_for_current_view(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.update_folder_subscriptions_action(is_mail_view);
    }

    pub(crate) fn update_folder_sort_order_action_for_current_view(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.update_folder_sort_order_action(is_mail_view);
    }

    fn update_mail_undelete_action(&self, mail_view_enabled: bool) {
        let enabled = mail_view_enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.can_undelete());
        if let Some(action) = self
            .lookup_action("undelete")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(enabled);
        }
    }

    pub(crate) fn update_mail_undelete_action_for_current_view(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.update_mail_undelete_action(is_mail_view);
    }

    fn update_mail_navigation_actions(&self, mail_view_enabled: bool) {
        let page = self.mail_page();
        let states = [
            (
                "mail-next",
                mail_view_enabled
                    && page
                        .as_ref()
                        .is_some_and(|page| page.can_select_relative_message(1)),
            ),
            (
                "mail-prev",
                mail_view_enabled
                    && page
                        .as_ref()
                        .is_some_and(|page| page.can_select_relative_message(-1)),
            ),
            (
                "mail-next-unread",
                mail_view_enabled && page.as_ref().is_some_and(|page| page.has_messages()),
            ),
            (
                "mail-prev-unread",
                mail_view_enabled && page.is_some_and(|page| page.has_messages()),
            ),
        ];
        for (name, enabled) in states {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
    }

    fn set_message_actions_enabled(&self, enabled: bool) {
        let enabled = enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.has_selected_message());
        for spec in ACTIONS
            .iter()
            .filter(|spec| spec.scope == ActionScope::MessageSelection)
        {
            let name = spec.action.trim_start_matches("win.");
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(enabled);
            }
        }
        if let Some(action) = self
            .lookup_action("reply-list")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(
                enabled
                    && self
                        .mail_page()
                        .is_some_and(|mail_page| mail_page.can_reply_list()),
            );
        }
        if let Some(action) = self
            .lookup_action("copy-message-link")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(
                enabled
                    && self
                        .mail_page()
                        .is_some_and(|mail_page| mail_page.can_copy_message_link()),
            );
        }
        if let Some(action) = self
            .lookup_action("goto-containing-folder")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(
                enabled
                    && self
                        .mail_page()
                        .is_some_and(|mail_page| mail_page.can_goto_containing_folder()),
            );
        }
        if let Some(action) = self
            .lookup_action("add-sender-contact")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(
                enabled
                    && self
                        .mail_page()
                        .is_some_and(|mail_page| mail_page.can_add_selected_sender()),
            );
        }
        let print_enabled = enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.print_action_enabled(MailPrintAction::Print));
        let preview_enabled = enabled
            && self
                .mail_page()
                .is_some_and(|mail_page| mail_page.print_action_enabled(MailPrintAction::Preview));
        for (name, action_enabled) in [
            ("mail-print", print_enabled),
            ("mail-print-preview", preview_enabled),
        ] {
            if let Some(action) = self
                .lookup_action(name)
                .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            {
                action.set_enabled(action_enabled);
            }
        }
        if let Some(action) = self
            .lookup_action("mail-print-cancel")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            let is_mail_view = self
                .imp()
                .view_stack
                .borrow()
                .as_ref()
                .and_then(|view_stack| view_stack.visible_child_name())
                .is_some_and(|view_name| view_name == "mail");
            action.set_enabled(
                is_mail_view
                    && self
                        .mail_page()
                        .is_some_and(|mail_page| mail_page.print_cancel_enabled()),
            );
        }
    }

    pub(crate) fn update_message_actions_for_current_view(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.set_message_actions_enabled(is_mail_view);
    }

    pub(crate) fn update_mail_print_actions(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        self.set_message_actions_enabled(is_mail_view);
    }

    fn setup_layout(&self) {
        let setup_started = std::time::Instant::now();
        tracing::debug!("Main window layout setup started");
        let view_stack = adw::ViewStack::new();
        view_stack.set_vexpand(true);
        view_stack.set_hexpand(true);
        view_stack.set_valign(gtk::Align::Fill);
        view_stack.set_halign(gtk::Align::Fill);
        view_stack.set_vhomogeneous(false);
        view_stack.set_hhomogeneous(false);

        let mail_page = MailPage::new();
        let mail_stack_page = view_stack.add_titled_with_icon(&mail_page, Some("mail"), "Mail", "mail-unread-symbolic");
        self.imp().mail_stack_page.replace(Some(mail_stack_page));
        let win_weak_unread = self.downgrade();
        mail_page.set_on_unread_count_changed(move |_count| {
            if let Some(win) = win_weak_unread.upgrade() {
                win.sync_unread_counter_and_title();
            }
        });
        tracing::debug!(
            elapsed_ms = setup_started.elapsed().as_millis() as u64,
            "Mail page added to view stack"
        );

        // Construct only the visible mail module at startup.  EDS/ECal/EBook
        // page construction starts native worker activity; eagerly creating
        // every hidden module can overwhelm a cold Linux EDS session before
        // the first frame has settled.
        let calendar_bin = Self::lazy_module_bin(&crate::i18n::gettext("Calendar"));
        view_stack.add_titled_with_icon(
            &calendar_bin,
            Some("calendar"),
            &crate::i18n::gettext("Calendar"),
            "x-office-calendar-symbolic",
        );

        let contacts_bin = Self::lazy_module_bin(&crate::i18n::gettext("Contacts"));
        view_stack.add_titled_with_icon(
            &contacts_bin,
            Some("contacts"),
            &crate::i18n::gettext("Contacts"),
            "x-office-address-book-symbolic",
        );

        let tasks_bin = Self::lazy_module_bin(&crate::i18n::gettext("Tasks"));
        view_stack.add_titled_with_icon(
            &tasks_bin,
            Some("tasks"),
            &crate::i18n::gettext("Tasks"),
            "view-list-bullet-symbolic",
        );

        let memos_bin = Self::lazy_module_bin(&crate::i18n::gettext("Memos"));
        view_stack.add_named(&memos_bin, Some("memos"));
        self.imp().view_stack.replace(Some(view_stack.clone()));

        let ensure_module_loaded: Rc<dyn Fn(&str)> = {
            let calendar_bin = calendar_bin.clone();
            let contacts_bin = contacts_bin.clone();
            let tasks_bin = tasks_bin.clone();
            let memos_bin = memos_bin.clone();
            let window_weak = self.downgrade();
            Rc::new(move |name| {
                let (is_mobile, is_tablet) = window_weak
                    .upgrade()
                    .map(|w| (w.imp().is_mobile.get(), w.imp().is_tablet.get()))
                    .unwrap_or((false, false));
                let (sidebar_pos, sidebar_vis) = window_weak
                    .upgrade()
                    .map(|w| {
                        let pos = w.sidebar_position_for_view(name);
                        (pos, w.imp().sidebar_visible.get())
                    })
                    .unwrap_or_else(|| {
                        let settings = AppSettings::try_new();
                        let pos = settings
                            .map(|s| s.mail_sidebar_position())
                            .unwrap_or(240)
                            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
                        let vis = AppSettings::try_new()
                            .map(|s| s.mail_show_sidebar())
                            .unwrap_or(true);
                        (pos, vis)
                    });
                let effective_vis = if is_mobile { false } else { sidebar_vis };
                match name {
                    "calendar"
                        if calendar_bin
                            .child()
                            .map(|child| child.is::<adw::StatusPage>())
                            .unwrap_or(true) =>
                    {
                        let page = CalendarPage::new();
                        page.set_screen_mode(is_mobile, is_tablet);
                        if let Some(win) = window_weak.upgrade() {
                            page.set_medium_mode(win.imp().is_medium.get());
                            win.attach_calendar_year_view_options_button(&page);
                        }
                        page.set_sidebar_position(sidebar_pos);
                        page.set_sidebar_visible(effective_vis);
                        calendar_bin.set_child(Some(&page));
                    }
                    "contacts"
                        if contacts_bin
                            .child()
                            .map(|child| child.is::<adw::StatusPage>())
                            .unwrap_or(true) =>
                    {
                        let page = ContactsPage::new();
                        page.set_screen_mode(is_mobile, is_tablet);
                        page.set_sidebar_position(sidebar_pos);
                        page.set_sidebar_visible(effective_vis);
                        if let Some(bar) = page.search_bar() {
                            if let Some(win) = window_weak.upgrade() {
                                if let Some(btn) = win.imp().search_button.borrow().as_ref() {
                                    let btn_clone = btn.clone();
                                    bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
                                        let active = bar.is_search_mode();
                                        if btn_clone.is_active() != active {
                                            btn_clone.set_active(active);
                                        }
                                    });
                                }
                            }
                        }
                        if let Some(win) = window_weak.upgrade() {
                            win.attach_contacts_view_options_button(&page);
                        }
                        contacts_bin.set_child(Some(&page));
                    }
                    "tasks"
                        if tasks_bin
                            .child()
                            .map(|child| child.is::<adw::StatusPage>())
                            .unwrap_or(true) =>
                    {
                        let page = crate::views::TasksPage::new();
                        page.set_screen_mode(is_mobile, is_tablet);
                        page.set_sidebar_position(sidebar_pos);
                        page.set_sidebar_visible(effective_vis);
                        if let Some(bar) = page.search_bar() {
                            if let Some(win) = window_weak.upgrade() {
                                if let Some(btn) = win.imp().search_button.borrow().as_ref() {
                                    let btn_clone = btn.clone();
                                    bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
                                        let active = bar.is_search_mode();
                                        if btn_clone.is_active() != active {
                                            btn_clone.set_active(active);
                                        }
                                    });
                                }
                            }
                        }
                        tasks_bin.set_child(Some(&page));
                    }
                    "memos"
                        if memos_bin
                            .child()
                            .map(|child| child.is::<adw::StatusPage>())
                            .unwrap_or(true) =>
                    {
                        let page = crate::views::MemosPage::new();
                        page.set_screen_mode(is_mobile, is_tablet);
                        page.set_sidebar_position(sidebar_pos);
                        page.set_sidebar_visible(effective_vis);
                        if let Some(bar) = page.search_bar() {
                            if let Some(win) = window_weak.upgrade() {
                                if let Some(btn) = win.imp().search_button.borrow().as_ref() {
                                    let btn_clone = btn.clone();
                                    bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
                                        let active = bar.is_search_mode();
                                        if btn_clone.is_active() != active {
                                            btn_clone.set_active(active);
                                        }
                                    });
                                }
                            }
                        }
                        memos_bin.set_child(Some(&page));
                    }
                    _ => {}
                }
            })
        };

        let sync_sidebar_position: Rc<dyn Fn(i32)> = {
            let mail_page = mail_page.clone();
            let calendar_bin = calendar_bin.clone();
            let contacts_bin = contacts_bin.clone();
            let tasks_bin = tasks_bin.clone();
            let memos_bin = memos_bin.clone();
            let win_weak_sidebar_pos = self.downgrade();
            Rc::new(move |pos: i32| {
                let clamped = pos.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
                mail_page.set_sidebar_position(clamped);
                if let Some(win) = win_weak_sidebar_pos.upgrade() {
                    if !win.imp().is_mobile.get() {
                        if let Some(sb_h) = win.imp().sidebar_header.borrow().as_ref() {
                            sb_h.set_size_request(clamped, -1);
                        }
                    }
                }
                if let Some(child) = calendar_bin.child() {
                    if let Ok(page) = child.downcast::<CalendarPage>() {
                        page.set_sidebar_position(clamped);
                    }
                }
                if let Some(child) = contacts_bin.child() {
                    if let Ok(page) = child.downcast::<ContactsPage>() {
                        page.set_sidebar_position(clamped);
                    }
                }
                if let Some(child) = tasks_bin.child() {
                    if let Ok(page) = child.downcast::<TasksPage>() {
                        page.set_sidebar_position(clamped);
                    }
                }
                if let Some(child) = memos_bin.child() {
                    if let Ok(page) = child.downcast::<MemosPage>() {
                        page.set_sidebar_position(clamped);
                    }
                }
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
            let pos = settings.mail_sidebar_position().clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
            sync_sidebar_position(pos);

            let sync_sidebar_position_for_settings = Rc::clone(&sync_sidebar_position);
            settings.connect_changed(Some("mail-sidebar-position"), move |settings, _| {
                let pos = settings.int("mail-sidebar-position").clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
                sync_sidebar_position_for_settings(pos);
            });
        }

        // Module construction must not depend on GSettings being installed.
        // A direct `cargo run` has historically shown the permanent placeholder
        // for contacts/tasks/memos because the persistence layer is optional in
        // development builds.  Keep persistence optional, but always replace
        // the placeholder when the user navigates to a module.
        let ensure_module_loaded_for_signal = Rc::clone(&ensure_module_loaded);
        let settings_for_view = AppSettings::try_new();
        let window_for_search_route = self.downgrade();
        view_stack.connect_visible_child_name_notify(move |stack| {
            if let Some(view_name) = stack.visible_child_name() {
                let view_str = view_name.as_str();
                if let Some(window) = window_for_search_route.upgrade() {
                    window.invalidate_search_route_for_visible_page(Some(view_str));
                    let title = match view_str {
                        "mail" => window
                            .mail_page()
                            .map(|m| m.active_folder_display_title())
                            .unwrap_or_else(|| crate::i18n::gettext("Inbox")),
                        "calendar" => crate::i18n::gettext("Calendar"),
                        "contacts" => crate::i18n::gettext("Contacts"),
                        "tasks" => crate::i18n::gettext("Tasks"),
                        "memos" => crate::i18n::gettext("Memos"),
                        _ => String::new(),
                    };
                    window.update_sidebar_title(&title);
                    let view_pos = window.sidebar_position_for_view(view_str);
                    if !window.imp().is_mobile.get() {
                        if let Some(sb_h) = window.imp().sidebar_header.borrow().as_ref() {
                            sb_h.set_size_request(view_pos, -1);
                        }
                    }
                    match view_str {
                        "contacts" => {
                            if let Some(c) = window.contacts_page() {
                                c.set_sidebar_position(view_pos);
                            }
                        }
                        "mail" => {
                            if let Some(m) = window.mail_page() {
                                m.set_sidebar_position(view_pos);
                            }
                        }
                        "calendar" => {
                            if let Some(c) = window.calendar_page() {
                                c.set_sidebar_position(view_pos);
                            }
                        }
                        "tasks" => {
                            if let Some(t) = window.tasks_page() {
                                t.set_sidebar_position(view_pos);
                            }
                        }
                        "memos" => {
                            if let Some(m) = window.memos_page() {
                                m.set_sidebar_position(view_pos);
                            }
                        }
                        _ => {}
                    }
                }
                ensure_module_loaded_for_signal(view_str);
                if let Some(window) = window_for_search_route.upgrade() {
                    window.set_screen_mode(window.imp().is_mobile.get(), window.imp().is_tablet.get());
                    window.sync_sidebar_state_for_current_view();
                    window.sync_right_sidebar_toggle();
                    window.sync_toolbar_state_for_current_view();
                }
                if let Some(settings) = settings_for_view.as_ref() {
                    settings.set_active_view(view_str);
                }
            } else if let Some(window) = window_for_search_route.upgrade() {
                window.invalidate_search_route_for_visible_page(None);
            }
        });

        let initial_sidebar_pos = self.sidebar_position_for_view(&active_view);
        let initial_sidebar_vis = AppSettings::try_new()
            .map(|s| s.mail_show_sidebar())
            .unwrap_or(true);

        let sidebar_header = adw::HeaderBar::builder()
            .show_start_title_buttons(true)
            .show_end_title_buttons(false)
            .css_classes(vec!["flat", "sidebar-header"])
            .build();
        sidebar_header.set_overflow(gtk::Overflow::Hidden);
        let sidebar_title = adw::WindowTitle::builder()
            .title(&crate::i18n::gettext("Inbox"))
            .build();
        sidebar_header.set_title_widget(Some(&sidebar_title));
        sidebar_header.set_size_request(initial_sidebar_pos, -1);
        sidebar_header.set_visible(initial_sidebar_vis);
        self.imp().sidebar_header.replace(Some(sidebar_header.clone()));
        self.imp().sidebar_title.replace(Some(sidebar_title.clone()));

        let sidebar_close_btn = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(&crate::i18n::gettext("Collapse Sidebar (F9)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&sidebar_close_btn, &crate::i18n::gettext("Collapse Sidebar"));
        let win_weak_close = self.downgrade();
        sidebar_close_btn.connect_clicked(move |_| {
            if let Some(win) = win_weak_close.upgrade() {
                win.toggle_sidebar_animated(false);
            }
        });
        sidebar_header.pack_end(&sidebar_close_btn);
        self.imp().sidebar_close_button.replace(Some(sidebar_close_btn.clone()));

        let header_bar = adw::HeaderBar::builder()
            .show_start_title_buttons(!initial_sidebar_vis)
            .show_end_title_buttons(true)
            .hexpand(true)
            .build();
        self.imp().header_bar.replace(Some(header_bar.clone()));

        let restore_sidebar_btn = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(&crate::i18n::gettext("Show Sidebar (F9)"))
            .css_classes(vec!["flat"])
            .visible(!initial_sidebar_vis)
            .build();
        set_accessible_label(&restore_sidebar_btn, &crate::i18n::gettext("Show Sidebar"));
        let win_weak_restore = self.downgrade();
        restore_sidebar_btn.connect_clicked(move |_| {
            if let Some(win) = win_weak_restore.upgrade() {
                if win.imp().is_mobile.get() && win.active_view_name() == "mail" {
                    if let Some(mail) = win.mail_page() {
                        match mail.drill_down_level() {
                            crate::views::MailDrillDownLevel::Folders => {
                                mail.set_drill_down_level(crate::views::MailDrillDownLevel::Messages);
                            }
                            _ => {
                                mail.set_drill_down_level(crate::views::MailDrillDownLevel::Folders);
                            }
                        }
                    }
                } else {
                    let is_vis = win.is_sidebar_visible_for_active_view();
                    win.toggle_sidebar_animated(!is_vis);
                }
            }
        });
        self.imp().restore_sidebar_button.replace(Some(restore_sidebar_btn.clone()));

        if let Some(settings) = AppSettings::try_new() {
            let win_weak_vis = self.downgrade();
            settings.connect_changed(Some("mail-show-sidebar"), move |settings, _| {
                let vis = settings.boolean("mail-show-sidebar");
                if let Some(win) = win_weak_vis.upgrade() {
                    if win.is_sidebar_visible_for_active_view() != vis {
                        win.toggle_sidebar_animated(vis);
                    }
                }
            });
        }

        let search_button = gtk::ToggleButton::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text(&crate::i18n::gettext("Search (Ctrl+F)"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&search_button, &crate::i18n::gettext("Search"));
        self.imp().search_button.replace(Some(search_button.clone()));

        let win_for_search = self.downgrade();
        search_button.connect_toggled(move |btn| {
            if let Some(win) = win_for_search.upgrade() {
                let is_active = btn.is_active();
                let current_search_active = win.is_active_search_open();
                if is_active != current_search_active {
                    if is_active {
                        win.focus_active_search();
                    } else {
                        win.close_active_search();
                    }
                }
            }
        });

        let search_btn_for_mail = search_button.clone();
        if let Some(bar) = mail_page.search_bar() {
            bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
                let active = bar.is_search_mode();
                if search_btn_for_mail.is_active() != active {
                    search_btn_for_mail.set_active(active);
                }
            });
        }

        let win_for_stack = self.downgrade();
        let search_btn_for_stack = search_button.clone();
        view_stack.connect_visible_child_name_notify(move |_| {
            if let Some(win) = win_for_stack.upgrade() {
                let is_open = win.is_active_search_open();
                if search_btn_for_stack.is_active() != is_open {
                    search_btn_for_stack.set_active(is_open);
                }
                win.sync_back_button();
            }
        });

        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(&crate::i18n::gettext("Back"))
            .visible(false)
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&back_button, &crate::i18n::gettext("Back"));
        let win_weak_for_back = self.downgrade();
        back_button.connect_clicked(move |_| {
            if let Some(win) = win_weak_for_back.upgrade() {
                win.handle_navigate_back();
            }
        });
        self.imp().back_button.replace(Some(back_button.clone()));

        header_bar.pack_start(&back_button);
        header_bar.pack_start(&restore_sidebar_btn);
        header_bar.pack_start(&search_button);

        let compose_button = gtk::Button::builder()
            .action_name("app.compose")
            .css_classes(vec!["suggested-action"])
            .build();

        Self::configure_primary_action(&compose_button, &active_view, false);
        self.imp().compose_button.replace(Some(compose_button.clone()));
        let primary_action_for_view = compose_button.clone();
        let win_weak_primary = self.downgrade();
        view_stack.connect_visible_child_name_notify(move |stack| {
            if let Some(view_name) = stack.visible_child_name() {
                let is_compact = win_weak_primary
                    .upgrade()
                    .map(|w| w.imp().is_mobile.get() || w.imp().is_tablet.get() || w.imp().is_medium.get())
                    .unwrap_or(false);
                Self::configure_primary_action(&primary_action_for_view, view_name.as_str(), is_compact);
            }
        });

        header_bar.pack_start(&compose_button);

        let window_title = adw::WindowTitle::builder()
            .title("")
            .build();
        self.imp().window_title.replace(Some(window_title.clone()));

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        self.imp().view_switcher.replace(Some(view_switcher.clone()));

        let update_title_widget = {
            let header_bar = header_bar.clone();
            let window_title = window_title.clone();
            let view_switcher = view_switcher.clone();
            let win_weak = self.downgrade();
            move || {
                let is_narrow = win_weak
                    .upgrade()
                    .map(|w| w.imp().is_mobile.get() || w.imp().is_tablet.get())
                    .unwrap_or(false);
                if is_narrow {
                    header_bar.set_title_widget(Some(&window_title));
                } else {
                    let is_top = AppSettings::try_new()
                        .map(|s| s.switcher_location() == "top")
                        .unwrap_or(false);
                    if is_top {
                        header_bar.set_title_widget(Some(&view_switcher));
                    } else {
                        header_bar.set_title_widget(Some(&window_title));
                    }
                }
            }
        };

        update_title_widget();

        if let Some(settings) = AppSettings::try_new() {
            let update_cb = update_title_widget.clone();
            settings.connect_changed(Some("switcher-location"), move |_, _| {
                update_cb();
            });
            let win_weak_counter = self.downgrade();
            settings.connect_changed(Some("show-window-title-unread-counter"), move |_, _| {
                if let Some(win) = win_weak_counter.upgrade() {
                    win.sync_unread_counter_and_title();
                }
            });
        }

        let menu_model = Self::build_primary_menu();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu_model)
            .tooltip_text(&crate::i18n::gettext("Main Menu"))
            .primary(true)
            .build();

        let right_sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle To Do bar for today"))
            .css_classes(vec!["flat"])
            .build();
        set_accessible_label(&right_sidebar_toggle, &crate::i18n::gettext("To Do bar for today"));
        let win_weak_right = self.downgrade();
        right_sidebar_toggle.connect_toggled(move |btn| {
            if let Some(win) = win_weak_right.upgrade() {
                let active = btn.is_active();
                if active != win.is_active_view_right_panel_visible() {
                    win.set_active_view_right_panel_visible(active);
                }
            }
        });
        self.imp().right_sidebar_toggle.replace(Some(right_sidebar_toggle.clone()));

        let agenda_preview_toggle = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text(&crate::i18n::gettext("Toggle event details (Ctrl+Shift+D)"))
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        set_accessible_label(&agenda_preview_toggle, &crate::i18n::gettext("Event details"));
        let win_weak_agenda = self.downgrade();
        agenda_preview_toggle.connect_toggled(move |btn| {
            if let Some(win) = win_weak_agenda.upgrade() {
                if let Some(page) = win.calendar_page() {
                    let active = btn.is_active();
                    if active != page.is_agenda_detail_visible() {
                        page.set_agenda_detail_visible(active);
                    }
                }
            }
        });
        self.imp().agenda_preview_toggle.replace(Some(agenda_preview_toggle.clone()));

        header_bar.pack_end(&menu_button);
        header_bar.pack_end(&right_sidebar_toggle);
        header_bar.pack_end(&agenda_preview_toggle);
        if let Some(view_btn) = mail_page.view_options_split_button() {
            let is_initially_mail = view_stack.visible_child_name().as_deref() == Some("mail");
            view_btn.set_visible(is_initially_mail);
            header_bar.pack_end(&view_btn);
            self.imp()
                .mail_view_options_button
                .replace(Some(view_btn.clone()));
            let btn_weak = view_btn.downgrade();
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(btn) = btn_weak.upgrade() {
                    let is_mail = stack.visible_child_name().as_deref() == Some("mail");
                    btn.set_visible(is_mail);
                }
            });
        }
        if let Some(filter_btn) = mail_page.filter_options_button() {
            let is_initially_mail = view_stack.visible_child_name().as_deref() == Some("mail");
            filter_btn.set_visible(is_initially_mail);
            header_bar.pack_end(&filter_btn);
            self.imp()
                .mail_filter_options_button
                .replace(Some(filter_btn.clone()));
            let btn_weak = filter_btn.downgrade();
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(btn) = btn_weak.upgrade() {
                    let is_mail = stack.visible_child_name().as_deref() == Some("mail");
                    btn.set_visible(is_mail);
                }
            });
        }

        if let Some(contacts) = self.contacts_page() {
            self.attach_contacts_view_options_button(&contacts);
        }
        if let Some(cal) = self.calendar_page() {
            self.attach_calendar_year_view_options_button(&cal);
        }

        if let Some(settings) = AppSettings::try_new() {
            let win_weak_right_sync = self.downgrade();
            for key in &[
                "mail-show-todo-bar",
                "calendar-show-tag-vpane",
                "calendar-show-agenda-preview",
                "contact-preview-visible",
                "task-preview-visible",
                "memo-preview-visible",
            ] {
                let win_weak_k = win_weak_right_sync.clone();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(win) = win_weak_k.upgrade() {
                        win.sync_right_sidebar_toggle();
                    }
                });
            }

            let win_weak_tb_sync = self.downgrade();
            for key in &[
                "mail-show-toolbar",
                "contact-show-toolbar",
                "task-show-toolbar",
                "memo-show-toolbar",
            ] {
                let win_weak_k = win_weak_tb_sync.clone();
                settings.connect_changed(Some(key), move |_, _| {
                    if let Some(win) = win_weak_k.upgrade() {
                        win.sync_toolbar_state_for_current_view();
                    }
                });
            }
        }
        self.sync_sidebar_state_for_current_view();
        self.sync_right_sidebar_toggle();
        self.sync_toolbar_state_for_current_view();

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
            .icon_name("network-idle-symbolic")
            .tooltip_text(&crate::i18n::gettext("Online status (Toggle online/offline)"))
            .css_classes(vec!["flat", "circular"])
            .build();

        let status_label = gtk::Label::builder()
            .label(&format!("Meridian  ·  {}", crate::i18n::gettext("Ready")))
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(10)
            .build();

        let status_progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .pulse_step(0.1)
            .build();

        if let Some(settings) = AppSettings::try_new() {
            statusbar_box.set_visible(settings.mail_show_statusbar());
            let settings_for_online = settings.clone();
            let window_for_online = self.downgrade();
            online_toggle.connect_clicked(move |_| {
                let offline = !settings_for_online.offline_mode();
                if let Some(window) = window_for_online.upgrade() {
                    window.set_offline_mode(offline);
                }
            });
            let window_for_signal = self.downgrade();
            settings.connect_changed(Some("offline-mode"), move |settings, _| {
                if let Some(window) = window_for_signal.upgrade() {
                    window.sync_transport_mode(settings.boolean("offline-mode"), false);
                }
            });
            let statusbar_for_setting = statusbar_box.clone();
            settings.connect_changed(Some("mail-show-statusbar"), move |settings, _| {
                statusbar_for_setting.set_visible(settings.boolean("mail-show-statusbar"));
            });
        } else {
            online_toggle.set_sensitive(false);
        }

        let date_label = gtk::Label::builder()
            .label(&format_current_status_date())
            .css_classes(vec!["caption", "dim-label"])
            .halign(gtk::Align::End)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(10)
            .build();
        let date_label_weak = date_label.downgrade();
        glib::timeout_add_seconds_local(60, move || {
            if let Some(lbl) = date_label_weak.upgrade() {
                lbl.set_label(&format_current_status_date());
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });

        let debug_toggle = gtk::Button::builder()
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text(&crate::i18n::gettext("Show / Hide Developer Log Console (Ctrl+F12)"))
            .css_classes(vec!["flat", "circular"])
            .visible(true)
            .build();

        let debug_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(250)
            .reveal_child(false)
            .vexpand(false)
            .valign(gtk::Align::End)
            .build();
        let debug_panel = build_debug_console_panel(&debug_revealer);
        debug_revealer.set_child(Some(&debug_panel));

        let window_for_debug_toggle = self.downgrade();
        debug_toggle.connect_clicked(move |_| {
            if let Some(win) = window_for_debug_toggle.upgrade() {
                win.toggle_debug_console();
            }
        });

        if let Some(settings) = AppSettings::try_new() {
            let enabled = settings.debug_console_enabled();
            debug_revealer.set_visible(enabled);
            debug_revealer.set_reveal_child(enabled);
            let debug_revealer_for_setting = debug_revealer.clone();
            settings.connect_changed(Some("debug-console-enabled"), move |settings, _| {
                let enabled = settings.boolean("debug-console-enabled");
                if enabled {
                    debug_revealer_for_setting.set_visible(true);
                }
                debug_revealer_for_setting.set_reveal_child(enabled);
            });
        } else {
            debug_revealer.set_visible(false);
        }
        debug_revealer.connect_child_revealed_notify(move |revealer| {
            if !revealer.reveals_child() && !revealer.is_child_revealed() {
                revealer.set_visible(false);
            }
        });

        statusbar_box.append(&online_toggle);
        statusbar_box.append(&status_label);
        statusbar_box.append(&status_progress);
        statusbar_box.append(&date_label);
        statusbar_box.append(&debug_toggle);

        self.imp().status_label.replace(Some(status_label.clone()));
        self.imp()
            .status_progress
            .replace(Some(status_progress.clone()));
        self.imp()
            .online_toggle
            .replace(Some(online_toggle.clone()));
        self.imp()
            .debug_revealer
            .replace(Some(debug_revealer.clone()));
        self.imp()
            .statusbar_box
            .replace(Some(statusbar_box.clone()));
        let initial_transport_snapshot = self.imp().transport_service.set_mode(
            if AppSettings::try_new().is_some_and(|settings| settings.offline_mode()) {
                MailTransportMode::Offline
            } else {
                MailTransportMode::Online
            },
        );
        self.imp()
            .transport_generation
            .set(initial_transport_snapshot.generation);
        self.update_transport_presentation(&initial_transport_snapshot);
        let window_for_transport_status = self.downgrade();
        let transport_status_source = glib::timeout_add_seconds_local(1, move || {
            let Some(window) = window_for_transport_status.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let snapshot = window.imp().transport_service.snapshot();
            if snapshot.queued_items != window.imp().transport_queued_items.get() {
                window.update_transport_presentation(&snapshot);
            }
            glib::ControlFlow::Continue
        });
        self.imp()
            .transport_status_source
            .replace(Some(transport_status_source));

        let view_switcher_bar = adw::ViewSwitcherBar::builder()
            .stack(&view_stack)
            .reveal(false)
            .build();
        self.imp()
            .view_switcher_bar
            .replace(Some(view_switcher_bar.clone()));

        let mut switcher_badges = Vec::new();
        if let Some(badge) = Self::attach_mail_switcher_badge(&view_switcher, "Mail") {
            switcher_badges.push(badge);
        }
        if let Some(badge) = Self::attach_mail_switcher_badge(&view_switcher_bar, "Mail") {
            switcher_badges.push(badge);
        }
        self.imp().mail_switcher_badges.replace(switcher_badges);

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .css_classes(vec!["window-header-box"])
            .build();
        header_box.append(&sidebar_header);
        header_box.append(&header_bar);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_box);
        toolbar_view.add_bottom_bar(&view_switcher_bar);
        toolbar_view.add_bottom_bar(&debug_revealer);
        toolbar_view.add_bottom_bar(&statusbar_box);
        toolbar_view.set_content(Some(&view_stack));

        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.set_hexpand(true);
        toast_overlay.set_vexpand(true);
        toast_overlay.set_child(Some(&toolbar_view));

        // Responsive Breakpoints: 5-Tier Model (matching GNOME HIG & Convey)
        // 1. Desktop Large (> 1050sp): 3 columns, full text labels in switcher and action button
        // 2. Desktop Medium (860sp - 1050sp): 3 columns, compact icon-only switcher and + action button
        // 3. Tablet / Split-Screen (650sp - 860sp): 2 columns, split headerbars, icon-only switcher and + action button
        // 4. Mobile (400sp - 650sp): 1 column drilldown with bottom view switcher bar & single unified headerbar
        // 5. Compact Mobile (<= 400sp): 1 column drilldown with compact controls (e.g. PinePhone 360px)
        let bp_medium =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1050sp").unwrap());
        let bp_tablet =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        let bp_mobile =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 650sp").unwrap());
        let bp_compact =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 400sp").unwrap());

        // Note: sidebar_header is ONLY hidden in mobile/compact mode (where 1-column drilldown is used).
        // In tablet and desktop modes, the sidebar is a visible side-by-side pane and retains its matching sidebar_header.
        bp_mobile.add_setter(&sidebar_header, "visible", Some(&false.to_value()));
        bp_compact.add_setter(&sidebar_header, "visible", Some(&false.to_value()));

        bp_tablet.add_setter(&view_switcher_bar, "reveal", Some(&true.to_value()));
        bp_mobile.add_setter(&view_switcher_bar, "reveal", Some(&true.to_value()));
        bp_compact.add_setter(&view_switcher_bar, "reveal", Some(&true.to_value()));

        let win_weak_med_apply = self.downgrade();
        bp_medium.connect_apply(move |_| {
            if let Some(win) = win_weak_med_apply.upgrade() {
                win.set_medium_screen(true);
            }
        });

        let win_weak_med_unapply = self.downgrade();
        bp_medium.connect_unapply(move |_| {
            if let Some(win) = win_weak_med_unapply.upgrade() {
                win.set_medium_screen(false);
            }
        });

        let win_weak_tab_apply = self.downgrade();
        bp_tablet.connect_apply(move |_| {
            if let Some(win) = win_weak_tab_apply.upgrade() {
                win.set_screen_mode(false, true);
                win.set_compact_screen(false);
            }
        });

        let win_weak_mob_apply = self.downgrade();
        bp_mobile.connect_apply(move |_| {
            if let Some(win) = win_weak_mob_apply.upgrade() {
                win.set_screen_mode(true, false);
                win.set_compact_screen(false);
            }
        });

        let win_weak_compact_apply = self.downgrade();
        bp_compact.connect_apply(move |_| {
            if let Some(win) = win_weak_compact_apply.upgrade() {
                win.set_screen_mode(true, false);
                win.set_compact_screen(true);
            }
        });

        // Libadwaita rule: If multiple breakpoints match, the LAST added breakpoint wins.
        // Therefore, broader breakpoints (medium 1050sp, tablet 860sp) must be added first, and more
        // specific breakpoints (mobile 650sp, compact 400sp) must be added later.
        self.add_breakpoint(bp_medium.clone());
        self.add_breakpoint(bp_tablet.clone());
        self.add_breakpoint(bp_mobile.clone());
        self.add_breakpoint(bp_compact.clone());

        // Connect notify::current-breakpoint as the central source of truth (as in GNOME Calendar)
        let bp_med_clone = bp_medium.clone();
        let bp_tab_clone = bp_tablet.clone();
        let bp_mob_clone = bp_mobile.clone();
        let bp_comp_clone = bp_compact.clone();
        self.connect_current_breakpoint_notify(move |win| {
            let current = win.current_breakpoint();
            if current.as_ref() == Some(&bp_comp_clone) {
                win.set_medium_screen(true);
                win.set_screen_mode(true, false);
                win.set_compact_screen(true);
            } else if current.as_ref() == Some(&bp_mob_clone) {
                win.set_medium_screen(true);
                win.set_screen_mode(true, false);
                win.set_compact_screen(false);
            } else if current.as_ref() == Some(&bp_tab_clone) {
                win.set_medium_screen(true);
                win.set_screen_mode(false, true);
                win.set_compact_screen(false);
            } else if current.as_ref() == Some(&bp_med_clone) {
                win.set_medium_screen(true);
                win.set_screen_mode(false, false);
                win.set_compact_screen(false);
            } else {
                win.set_medium_screen(false);
                win.set_screen_mode(false, false);
                win.set_compact_screen(false);
            }
        });

        // Hardware mouse back button (button 8 / XButton1)
        let mouse_back = gtk::GestureClick::new();
        mouse_back.set_button(8);
        let win_weak_mouse = self.downgrade();
        mouse_back.connect_pressed(move |gesture, _n_press, _x, _y| {
            if let Some(win) = win_weak_mouse.upgrade() {
                if win.handle_navigate_back() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        self.add_controller(mouse_back);

        // Touch swipe-to-back gesture (swipe right)
        let swipe = gtk::GestureSwipe::new();
        let win_weak_swipe = self.downgrade();
        swipe.connect_swipe(move |gesture, velocity_x, velocity_y| {
            if velocity_x > 200.0 && velocity_x.abs() > velocity_y.abs() * 1.5 {
                if let Some(win) = win_weak_swipe.upgrade() {
                    if win.handle_navigate_back() {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                }
            }
        });
        self.add_controller(swipe);

        let key_controller = gtk::EventControllerKey::new();
        let win_weak_for_key = self.downgrade();
        key_controller.connect_key_pressed(move |_, keyval, _, modifier| {
            if keyval == gdk::Key::Escape
                || (keyval == gdk::Key::Left && modifier.contains(gdk::ModifierType::ALT_MASK))
            {
                if let Some(win) = win_weak_for_key.upgrade() {
                    if win.handle_navigate_back() {
                        return glib::Propagation::Stop;
                    }
                }
            }
            if keyval == gdk::Key::question
                && !modifier.contains(gdk::ModifierType::CONTROL_MASK)
                && !modifier.contains(gdk::ModifierType::ALT_MASK)
            {
                if let Some(win) = win_weak_for_key.upgrade() {
                    let is_typing = gtk::prelude::GtkWindowExt::focus(&win)
                        .map(|w| w.is::<gtk::Editable>() || w.is::<gtk::TextView>())
                        .unwrap_or(false);
                    if !is_typing {
                        crate::dialogs::show_shortcuts_dialog_for_view(
                            Some(&win),
                            Some(&win.active_view_name()),
                        );
                        return glib::Propagation::Stop;
                    }
                }
            }
            if (keyval == gdk::Key::d || keyval == gdk::Key::D)
                && modifier.contains(gdk::ModifierType::CONTROL_MASK)
                && modifier.contains(gdk::ModifierType::SHIFT_MASK)
            {
                if let Some(win) = win_weak_for_key.upgrade() {
                    if win.active_view_name().as_str() == "calendar" {
                        if let Some(page) = win.calendar_page() {
                            if page.current_calendar_view() == CalendarView::Agenda {
                                let cur = page.is_agenda_detail_visible();
                                page.set_agenda_detail_visible(!cur);
                                return glib::Propagation::Stop;
                            }
                        }
                    }
                }
            }
            glib::Propagation::Proceed
        });
        self.add_controller(key_controller);

        let win_weak_stack = self.downgrade();
        view_stack.connect_visible_child_name_notify(move |_| {
            if let Some(win) = win_weak_stack.upgrade() {
                win.sync_back_button();
            }
        });

        self.imp().view_stack.replace(Some(view_stack));
        self.set_content(Some(toast_overlay));
        self.sync_right_sidebar_toggle();
        tracing::debug!(
            elapsed_ms = setup_started.elapsed().as_millis() as u64,
            "Main window layout installed"
        );

        // Let the first frame become interactive before restoring a non-mail
        // module from settings.  Mail remains the fast default; a previously
        // selected calendar/contacts/tasks/memos view is initialized on the
        // next main-loop turn instead of inside window construction.
        let win_weak_init = self.downgrade();
        if active_view != "mail" {
            let ensure_module_loaded_for_initial = Rc::clone(&ensure_module_loaded);
            glib::idle_add_local_once(move || {
                ensure_module_loaded_for_initial(&active_view);
                if let Some(win) = win_weak_init.upgrade() {
                    win.sync_right_sidebar_toggle();
                }
            });
        } else {
            let ensure_module_loaded_for_calendar = Rc::clone(&ensure_module_loaded);
            glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
                ensure_module_loaded_for_calendar("calendar");
                if let Some(win) = win_weak_init.upgrade() {
                    win.sync_right_sidebar_toggle();
                }
            });
        }
    }

    fn attach_mail_switcher_badge(container: &impl IsA<gtk::Widget>, title: &str) -> Option<gtk::Label> {
        fn find_mail_wide_box(widget: &gtk::Widget, title: &str) -> Option<gtk::Box> {
            if let Some(b) = widget.downcast_ref::<gtk::Box>() {
                if b.has_css_class("wide") {
                    let mut child = b.first_child();
                    while let Some(c) = child {
                        if let Some(lbl) = c.downcast_ref::<gtk::Label>() {
                            if lbl.label() == title {
                                return Some(b.clone());
                            }
                        }
                        child = c.next_sibling();
                    }
                }
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                if let Some(found) = find_mail_wide_box(&c, title) {
                    return Some(found);
                }
                child = c.next_sibling();
            }
            None
        }

        let wide_box = find_mail_wide_box(container.as_ref(), title)?;
        let badge = gtk::Label::builder()
            .css_classes(vec!["mail-tab-badge"])
            .visible(false)
            .valign(gtk::Align::Center)
            .build();
        wide_box.append(&badge);
        Some(badge)
    }

    fn lazy_module_bin(title: &str) -> adw::Bin {
        let status = adw::StatusPage::builder()
            .title(title)
            .description(&crate::i18n::gettext("This section will be loaded when opened for the first time."))
            .icon_name("view-refresh-symbolic")
            .vexpand(true)
            .hexpand(true)
            .build();
        adw::Bin::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&status)
            .build()
    }

    fn build_primary_menu() -> gio::Menu {
        let menu = gio::Menu::new();

        // 1. Zoom controls (GNOME HIG display section)
        let zoom_section = gio::Menu::new();
        Self::append_contract_menu(&zoom_section, MenuPath::PrimaryZoom);
        menu.append_section(None, &zoom_section);

        // 2. Main data actions (Sync & Rules)
        let actions_section = gio::Menu::new();
        Self::append_contract_menu(&actions_section, MenuPath::PrimaryActions);
        menu.append_section(None, &actions_section);

        // 3. Application & Help (Preferences, Shortcuts, About)
        let app_section = gio::Menu::new();
        Self::append_contract_menu(&app_section, MenuPath::PrimaryApp);
        menu.append_section(None, &app_section);

        menu
    }

    #[doc(hidden)]
    #[allow(dead_code)]
    fn build_classic_message_menu() -> gio::Menu {
        let message_menu = gio::Menu::new();
        let goto_menu = gio::Menu::new();
        Self::append_contract_menu(&goto_menu, MenuPath::MessageGoto);
        message_menu.append_submenu(Some(&crate::i18n::gettext("Go to")), &goto_menu);
        Self::append_contract_menu(&message_menu, MenuPath::MessageBeforeForward);
        let forward_menu = gio::Menu::new();
        Self::append_contract_menu(&forward_menu, MenuPath::MessageForward);
        message_menu.append_submenu(Some(&crate::i18n::gettext("Forward")), &forward_menu);
        Self::append_contract_menu(&message_menu, MenuPath::MessageAfterForward);
        message_menu
    }

    #[allow(dead_code)]
    fn build_transport_menu() -> gio::Menu {
        let menu = gio::Menu::new();
        Self::append_contract_menu(&menu, MenuPath::Transport);
        menu
    }

    fn append_contract_menu(menu: &gio::Menu, path: MenuPath) {
        for item in menu_actions(path) {
            menu.append(Some(&crate::i18n::gettext(item.label)), Some(item.action));
        }
    }

    fn setup_mail_transport_actions(&self) {
        for transport_action in [
            MailTransportAction::SendReceive,
            MailTransportAction::ReceiveAll,
            MailTransportAction::SendAll,
        ] {
            let name = transport_action.action_id();
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| {
                    if transport_action == MailTransportAction::ReceiveAll
                        && AppSettings::try_new().map(|s| s.flush_outbox()).unwrap_or(false)
                    {
                        window.drain_transport_queue();
                    }
                    window.start_mail_transport(transport_action);
                }
            ));
            self.add_action(&action);
        }

        let action_stop = gio::SimpleAction::new(MAIL_STOP_ACTION_ID, None);
        action_stop.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                if !window.imp().transport_service.stop() {
                    let msg = gettext("No cancelable mail transfer is running");
                    window.add_toast(&adw::Toast::new(&msg));
                }
                window.update_transport_presentation(&window.imp().transport_service.snapshot());
            }
        ));
        self.add_action(&action_stop);

        let action_work_offline = gio::SimpleAction::new(WORK_OFFLINE_ACTION_ID, None);
        action_work_offline.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| window.set_offline_mode(true)
        ));
        self.add_action(&action_work_offline);

        let action_work_online = gio::SimpleAction::new(WORK_ONLINE_ACTION_ID, None);
        action_work_online.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| window.set_offline_mode(false)
        ));
        self.add_action(&action_work_online);

        let snapshot = self.imp().transport_service.snapshot();
        self.update_transport_presentation(&snapshot);
        self.sync_unread_counter_and_title();
    }

    fn setup_actions(&self) {
        self.setup_mail_transport_actions();
        let action_new = gio::SimpleAction::new("new", None);
        action_new.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.present_contextual_new()
        ));
        self.add_action(&action_new);

        let is_top = AppSettings::try_new()
            .map(|s| s.switcher_location() == "top")
            .unwrap_or(false);
        let action_toggle_switcher =
            gio::SimpleAction::new_stateful("toggle-top-switcher", None, &is_top.to_variant());
        let settings_clone = AppSettings::try_new();
        action_toggle_switcher.connect_activate(move |action, _| {
            let current = action
                .state()
                .and_then(|state| state.get::<bool>())
                .unwrap_or(false);
            let next = !current;
            action.set_state(&next.to_variant());
            if let Some(s) = settings_clone.as_ref() {
                s.set_switcher_location(if next { "top" } else { "bottom-left" });
            }
        });
        if let Some(settings) = AppSettings::try_new() {
            let action_for_settings = action_toggle_switcher.clone();
            settings.connect_changed(Some("switcher-location"), move |settings, _| {
                action_for_settings.set_state(
                    &(settings.string("switcher-location").as_str() == "top").to_variant(),
                );
            });
        }
        self.add_action(&action_toggle_switcher);

        let action_preferences = gio::SimpleAction::new("preferences", None);
        action_preferences.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::PreferencesDialog::new();
                let win_clone = win.clone();
                dialog.connect_closed(move |_| {
                    win_clone.reorder_account_roots();
                });
                dialog.present(Some(&win));
            }
        ));
        self.add_action(&action_preferences);

        let action_toggle_debug = gio::SimpleAction::new("toggle-debug-console", None);
        let win_weak_debug = self.downgrade();
        action_toggle_debug.connect_activate(move |_, _| {
            if let Some(win) = win_weak_debug.upgrade() {
                win.toggle_debug_console();
            }
        });
        self.add_action(&action_toggle_debug);

        let initial_sb_vis = self.is_sidebar_visible_for_active_view();
        let action_sidebar = gio::SimpleAction::new_stateful(
            "toggle-sidebar",
            None,
            &initial_sb_vis.to_variant(),
        );
        let win_weak_sb_state = self.downgrade();
        action_sidebar.connect_change_state(move |action, target_state| {
            if let Some(target_state) = target_state {
                if let Some(vis) = target_state.get::<bool>() {
                    action.set_state(&vis.to_variant());
                    if let Some(win) = win_weak_sb_state.upgrade() {
                        win.toggle_sidebar_animated(vis);
                    }
                }
            }
        });
        let win_weak_sb_act = self.downgrade();
        action_sidebar.connect_activate(move |action, _| {
            let current = if let Some(win) = win_weak_sb_act.upgrade() {
                win.is_sidebar_visible_for_active_view()
            } else {
                action.state().and_then(|v| v.get::<bool>()).unwrap_or(true)
            };
            action.change_state(&(!current).to_variant());
        });
        self.add_action(&action_sidebar);
        self.add_boolean_setting_action(
            "toggle-preview",
            "show-message-preview",
            AppSettings::show_message_preview,
            AppSettings::set_show_message_preview,
        );
        self.add_boolean_setting_action(
            "toggle-all-headers",
            "mail-show-all-headers",
            AppSettings::mail_show_all_headers,
            AppSettings::set_mail_show_all_headers,
        );
        self.add_boolean_setting_action(
            "toggle-mail-threading",
            "mail-threading",
            AppSettings::mail_threading,
            AppSettings::set_mail_threading,
        );
        self.add_boolean_setting_action(
            "toggle-mail-show-deleted",
            "mail-show-deleted",
            AppSettings::mail_show_deleted,
            AppSettings::set_mail_show_deleted,
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
            "toggle-mail-status",
            "mail-show-status",
            AppSettings::mail_show_status,
            AppSettings::set_mail_show_status,
        );
        self.add_boolean_setting_action(
            "toggle-mail-flagged",
            "mail-show-flagged",
            AppSettings::mail_show_flagged,
            AppSettings::set_mail_show_flagged,
        );
        self.add_boolean_setting_action(
            "toggle-mail-follow-up",
            "mail-show-follow-up",
            AppSettings::mail_show_follow_up,
            AppSettings::set_mail_show_follow_up,
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

        self.add_boolean_setting_action(
            "use-vertical-mail-layout",
            "mail-layout",
            |settings| settings.mail_layout() == "vertical",
            |settings, is_vertical| {
                settings.set_mail_layout(if is_vertical { "vertical" } else { "horizontal" });
            },
        );
        self.add_boolean_setting_action(
            "use-horizontal-mail-layout",
            "mail-layout",
            |settings| settings.mail_layout() == "horizontal",
            |settings, is_horizontal| {
                settings.set_mail_layout(if is_horizontal { "horizontal" } else { "vertical" });
            },
        );
        self.add_boolean_setting_action(
            "use-classic-mail-layout",
            "mail-layout",
            |settings| settings.mail_layout() == "horizontal" || settings.mail_layout() == "classic",
            |settings, is_classic| {
                settings.set_mail_layout(if is_classic { "horizontal" } else { "vertical" });
            },
        );
        self.add_boolean_setting_action(
            "use-modern-mail-layout",
            "mail-layout",
            |settings| settings.mail_layout() == "vertical" || settings.mail_layout() == "modern",
            |settings, is_modern| {
                settings.set_mail_layout(if is_modern { "vertical" } else { "horizontal" });
            },
        );
        self.add_boolean_setting_action(
            "toggle-todo-bar",
            "mail-show-todo-bar",
            AppSettings::mail_show_todo_bar,
            AppSettings::set_mail_show_todo_bar,
        );

        self.add_boolean_setting_action(
            "toggle-mail-toolbar",
            "mail-show-toolbar",
            AppSettings::mail_show_toolbar,
            AppSettings::set_mail_show_toolbar,
        );

        let initial_tb_vis = self.is_toolbar_visible_for_active_view();
        let action_toolbar = gio::SimpleAction::new_stateful(
            "toggle-toolbar",
            None,
            &initial_tb_vis.to_variant(),
        );
        let win_weak_tb_state = self.downgrade();
        action_toolbar.connect_change_state(move |action, target_state| {
            if let Some(target_state) = target_state {
                if let Some(vis) = target_state.get::<bool>() {
                    action.set_state(&vis.to_variant());
                    if let Some(win) = win_weak_tb_state.upgrade() {
                        win.set_all_views_toolbar_visible(vis);
                    }
                }
            }
        });
        let win_weak_tb_act = self.downgrade();
        action_toolbar.connect_activate(move |action, _| {
            let current = action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            let next = !current;
            action.set_state(&next.to_variant());
            if let Some(win) = win_weak_tb_act.upgrade() {
                win.set_all_views_toolbar_visible(next);
            }
        });
        self.add_action(&action_toolbar);
        self.add_boolean_setting_action(
            "toggle-mail-statusbar",
            "mail-show-statusbar",
            AppSettings::mail_show_statusbar,
            AppSettings::set_mail_show_statusbar,
        );
        self.add_boolean_setting_action(
            "toggle-statusbar",
            "mail-show-statusbar",
            AppSettings::mail_show_statusbar,
            AppSettings::set_mail_show_statusbar,
        );
        self.add_boolean_setting_action(
            "toggle-debug-console",
            "debug-console-enabled",
            AppSettings::debug_console_enabled,
            AppSettings::set_debug_console_enabled,
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
                    Some(Ok(true)) => gettext("Undid last mail action"),
                    Some(Ok(false)) => gettext("No mail action to undo"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail undo operation was not applied");
                        gettext("Undo not possible: {error}").replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_undo);

        let action_undelete = gio::SimpleAction::new("undelete", None);
        action_undelete.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win.mail_page().map(|mail_page| mail_page.undelete());
                let message = match result {
                    Some(Ok(true)) => gettext("Restoring deleted message"),
                    Some(Ok(false)) => gettext("No deletion to undo"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail undelete operation was not applied");
                        gettext("Undelete not possible: {error}").replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
                };
                win.add_toast(&adw::Toast::new(&message));
                win.update_mail_undelete_action_for_current_view();
            }
        ));
        self.add_action(&action_undelete);

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
                    Some(Ok(true)) => gettext("Redid last mail action"),
                    Some(Ok(false)) => gettext("No mail action to redo"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail redo operation was not applied");
                        gettext("Redo not possible: {error}").replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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

        let action_reply_list = gio::SimpleAction::new("reply-list", None);
        action_reply_list.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.reply_list_selected() {
                        tracing::debug!("Reply-list action ignored without a list recipient");
                    }
                }
            }
        ));
        self.add_action(&action_reply_list);

        let action_reply_alternative = gio::SimpleAction::new("reply-alternative", None);
        action_reply_alternative.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.reply_alternative_selected() {
                        tracing::debug!(
                            "Alternative-reply action ignored without a selected message"
                        );
                    }
                }
            }
        ));
        self.add_action(&action_reply_alternative);

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

        let action_forward_inline = gio::SimpleAction::new("forward-inline", None);
        action_forward_inline.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.forward_inline_selected() {
                        tracing::debug!("Forward-inline action ignored without a selected message");
                    }
                }
            }
        ));
        self.add_action(&action_forward_inline);

        let action_forward_quoted = gio::SimpleAction::new("forward-quoted", None);
        action_forward_quoted.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.forward_quoted_selected() {
                        tracing::debug!("Forward-quoted action ignored without a selected message");
                    }
                }
            }
        ));
        self.add_action(&action_forward_quoted);

        let action_forward_attached = gio::SimpleAction::new("forward-attached", None);
        action_forward_attached.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.forward_attached_selected() {
                        tracing::debug!(
                            "Forward-attached action ignored without a selected message"
                        );
                    }
                }
            }
        ));
        self.add_action(&action_forward_attached);

        let action_copy_message_link = gio::SimpleAction::new("copy-message-link", None);
        action_copy_message_link.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let copied = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.copy_selected_message_link());
                if !copied {
                    let msg = gettext("Selected message does not have a valid Message-ID");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_copy_message_link);

        let action_goto_containing_folder = gio::SimpleAction::new("goto-containing-folder", None);
        action_goto_containing_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let navigated = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.goto_selected_message_folder());
                if !navigated {
                    let msg = gettext("Original folder of this message is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_goto_containing_folder);

        let action_add_sender_contact = gio::SimpleAction::new("add-sender-contact", None);
        action_add_sender_contact.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let presented = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.present_add_selected_sender());
                if !presented {
                    let msg = gettext("Selected message does not have a valid sender address");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_add_sender_contact);

        let action_edit_as_new = gio::SimpleAction::new("edit-as-new", None);
        action_edit_as_new.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let presented = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.edit_selected_as_new());
                if !presented {
                    tracing::debug!("Edit-as-new action could not open a complete composer");
                }
            }
        ));
        self.add_action(&action_edit_as_new);

        let action_export = gio::SimpleAction::new("export", None);
        action_export.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let exported = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.export_selected());
                if !exported {
                    let msg = gettext("No email selected to export");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_export);

        let action_mail_print = gio::SimpleAction::new("mail-print", None);
        action_mail_print.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let started = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.start_print(MailPrintAction::Print));
                if !started {
                    tracing::debug!("Print action ignored without a selected message");
                }
            }
        ));
        self.add_action(&action_mail_print);

        let action_mail_print_preview = gio::SimpleAction::new("mail-print-preview", None);
        action_mail_print_preview.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let started = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.start_print(MailPrintAction::Preview));
                if !started {
                    tracing::debug!("Print preview action ignored without a selected message");
                }
            }
        ));
        self.add_action(&action_mail_print_preview);

        let action_mail_print_cancel = gio::SimpleAction::new("mail-print-cancel", None);
        action_mail_print_cancel.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if !win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.cancel_print())
                {
                    tracing::debug!("Print cancellation ignored without an active print job");
                }
            }
        ));
        self.add_action(&action_mail_print_cancel);

        let action_message_source = gio::SimpleAction::new("view-message-source", None);
        action_message_source.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let shown = win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.present_message_source());
                if !shown {
                    let msg = gettext("No message selected");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_message_source);

        let action_toggle_mail_light = gio::SimpleAction::new_stateful(
            "toggle-mail-light-mode",
            None,
            &false.to_variant(),
        );
        let win_weak_light_state = self.downgrade();
        action_toggle_mail_light.connect_change_state(move |action, target_state| {
            if let Some(target_state) = target_state {
                if let Some(force_light) = target_state.get::<bool>() {
                    action.set_state(&force_light.to_variant());
                    if let Some(win) = win_weak_light_state.upgrade() {
                        if let Some(mail_page) = win.mail_page() {
                            mail_page.set_preview_light_mode(force_light);
                        }
                    }
                }
            }
        });
        let win_weak_light_act = self.downgrade();
        action_toggle_mail_light.connect_activate(move |action, _| {
            let current = action
                .state()
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false);
            let next = !current;
            action.set_state(&next.to_variant());
            if let Some(win) = win_weak_light_act.upgrade() {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.set_preview_light_mode(next);
                }
            }
        });
        self.add_action(&action_toggle_mail_light);

        let action_delete = gio::SimpleAction::new("delete", None);
        action_delete.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let Some(mail_page) = win.mail_page() else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                    return;
                };
                if !mail_page.has_selected_message() {
                    let msg = gettext("No message selected");
                    win.add_toast(&adw::Toast::new(&msg));
                    return;
                }
                let moves_to_trash = AppSettings::try_new()
                    .map(|s| s.mail_delete_moves_to_trash())
                    .unwrap_or(true);
                let all_in_trash = mail_page.selected_messages_all_in_trash();
                if moves_to_trash && !all_in_trash {
                    let message = match mail_page.delete_selected() {
                        Ok(true) => gettext("Message moved to trash"),
                        Ok(false) => gettext("No message selected"),
                        Err(error) => {
                            tracing::warn!(%error, "Mail delete operation was not applied");
                            gettext("Delete not possible: {error}")
                                .replace("{error}", &error.to_string())
                        }
                    };
                    win.add_toast(&adw::Toast::new(&message));
                    return;
                }
                let win_for_action = win.clone();
                let heading = gettext("Delete message?");
                let body = gettext(
                    "The selected message will be permanently deleted or moved to Trash.",
                );
                let confirm = gettext("Delete");
                crate::dialogs::run_destructive_action(
                    &win,
                    &heading,
                    &body,
                    &confirm,
                    move || {
                        let result = win_for_action.mail_page().map(|mail_page| {
                            mail_page.delete_selected()
                        });
                        let message = match result {
                            Some(Ok(true)) => gettext("Message deleted"),
                            Some(Ok(false)) => gettext("No message selected"),
                            Some(Err(error)) => {
                                tracing::warn!(%error, "Mail delete operation was not applied");
                                gettext("Delete not possible: {error}")
                                    .replace("{error}", &error.to_string())
                            }
                            None => gettext("Mail view is unavailable"),
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
                    Some(Ok(true)) => gettext("Message marked as unread"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mark-unread operation was not applied");
                        gettext("Marking not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                    Some(Ok(true)) => gettext("Message marked as read"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mark-read operation was not applied");
                        gettext("Marking as read not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&win) {
                    if let Some(editable) = focus.downcast_ref::<gtk::Editable>() {
                        editable.select_region(0, -1);
                        return;
                    }
                    if let Some(text_view) = focus.downcast_ref::<gtk::TextView>() {
                        let buffer = text_view.buffer();
                        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
                        return;
                    }
                    let mut parent = focus.parent();
                    while let Some(p) = parent {
                        if let Some(editable) = p.downcast_ref::<gtk::Editable>() {
                            editable.select_region(0, -1);
                            return;
                        }
                        parent = p.parent();
                    }
                }

                let count = win
                    .mail_page()
                    .map(|mail_page| mail_page.select_all_visible_messages())
                    .unwrap_or(0);
                let msg = ngettext(
                    "{count} visible message selected",
                    "{count} visible messages selected",
                    count as u32,
                )
                .replace("{count}", &count.to_string());
                win.add_toast(&adw::Toast::new(&msg));
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
                    Some(Ok(true)) => gettext("Flag changed"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail flag operation was not applied");
                        gettext("Flagging not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                    Some(Ok(true)) => gettext("Importance flag changed"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail important operation was not applied");
                        gettext("Setting importance not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
                };
                win.add_toast(&adw::Toast::new(&message));
            }
        ));
        self.add_action(&action_toggle_important);

        let action_follow_up = gio::SimpleAction::new("mail-follow-up", None);
        action_follow_up.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if !win
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.edit_selected_follow_up())
                {
                    let msg = gettext("No message selected");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_follow_up);

        let action_edit_labels = gio::SimpleAction::new("edit-labels", None);
        action_edit_labels.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_label_editor();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_edit_labels);

        for (action_name, copy) in [("move-message", false), ("copy-message", true)] {
            let action = gio::SimpleAction::new(action_name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    if let Some(mail_page) = win.mail_page() {
                        mail_page.present_folder_destination_dialog(copy);
                    } else {
                        let msg = gettext("Mail view is unavailable");
                        win.add_toast(&adw::Toast::new(&msg));
                    }
                }
            ));
            self.add_action(&action);
        }

        let action_archive = gio::SimpleAction::new("archive", None);
        action_archive.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let result = win
                    .mail_page()
                    .map(|mail_page| mail_page.archive_selected());
                let message = match result {
                    Some(Ok(true)) => gettext("Message archived"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail archive operation was not applied");
                        gettext("Archiving not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                    Some(Ok(true)) => gettext("Message marked as spam"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail junk operation was not applied");
                        gettext("Marking as spam not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                    Some(Ok(true)) => gettext("Spam mark removed"),
                    Some(Ok(false)) => gettext("No message selected"),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Mail not-junk operation was not applied");
                        gettext("Removing spam mark not possible: {error}")
                            .replace("{error}", &error.to_string())
                    }
                    None => gettext("Mail view is unavailable"),
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
                crate::dialogs::show_shortcuts_dialog_for_view(
                    Some(&win),
                    Some(&win.active_view_name()),
                );
            }
        ));
        self.add_action(&action_shortcuts);

        let action_oof = gio::SimpleAction::new("oof-settings", None);
        action_oof.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::OutOfOfficeDialog::new();
                dialog.present(Some(&win));
            }
        ));
        self.add_action(&action_oof);

        let action_filter = gio::SimpleAction::new("filter-editor", None);
        action_filter.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::FilterEditorDialog::new();
                dialog.present(Some(&win));
            }
        ));
        self.add_action(&action_filter);

        let action_apply_filters = gio::SimpleAction::new("apply-filters", None);
        action_apply_filters.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.apply_filters();
                }
            }
        ));
        self.add_action(&action_apply_filters);

        let action_search_folder = gio::SimpleAction::new("search-folder-editor", None);
        action_search_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::SearchFolderDialog::new("");
                dialog.present(Some(&win));
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

        for (name, kind) in [
            ("open-event", ReminderComponentKind::Event),
            ("open-task", ReminderComponentKind::Task),
            ("open-memo", ReminderComponentKind::Memo),
            ("open-contact", ReminderComponentKind::Contact),
        ] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, parameter| {
                    let Some(uri) = parameter.and_then(|value| value.str()) else {
                        return;
                    };
                    let Ok(identity) = parse_reminder_deep_link(uri) else {
                        let msg = gettext("Invalid reminder link");
                        win.add_toast(&adw::Toast::new(&msg));
                        return;
                    };
                    if identity.kind != kind {
                        let msg = gettext("Reminder link does not match requested component");
                        win.add_toast(&adw::Toast::new(&msg));
                        return;
                    }
                    win.open_component(identity);
                }
            ));
            self.add_action(&action);
        }

        let action_new_folder = gio::SimpleAction::new("new-folder", None);
        action_new_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_new_folder_dialog();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_new_folder);

        let action_rename_folder = gio::SimpleAction::new("rename-folder", None);
        action_rename_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_rename_selected_folder();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_rename_folder);

        let action_delete_folder = gio::SimpleAction::new("delete-folder", None);
        action_delete_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_delete_selected_folder();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_delete_folder);

        let action_refresh_folder = gio::SimpleAction::new("refresh-folder", None);
        action_refresh_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.refresh_selected_folder();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_refresh_folder);

        let action_mark_all_read = gio::SimpleAction::new("mark-all-read", None);
        action_mark_all_read.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    match mail_page.mark_all_read_in_selected_folder() {
                        Ok(count) => {
                            if count > 0 {
                                let msg = ngettext(
                                    "{count} message marked as read",
                                    "{count} messages marked as read",
                                    count as u32,
                                )
                                .replace("{count}", &count.to_string());
                                win.add_toast(&adw::Toast::new(&msg));
                            } else {
                                let msg = gettext("No unread messages in folder");
                                win.add_toast(&adw::Toast::new(&msg));
                            }
                        }
                        Err(error) => {
                            let msg = gettext("Error marking messages: {error}")
                                .replace("{error}", &error.to_string());
                            win.add_toast(&adw::Toast::new(&msg));
                        }
                    }
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_mark_all_read);

        let action_next_unread_folder = gio::SimpleAction::new("next-unread-folder", None);
        action_next_unread_folder.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    if !mail_page.select_next_unread_folder() {
                        let msg = gettext("No more unread folders available");
                        win.add_toast(&adw::Toast::new(&msg));
                    }
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_next_unread_folder);

        let action_folder_properties = gio::SimpleAction::new("folder-properties", None);
        action_folder_properties.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_folder_properties_dialog();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        action_folder_properties.set_enabled(false);
        self.add_action(&action_folder_properties);

        let action_folder_subscriptions = gio::SimpleAction::new("folder-subscriptions", None);
        action_folder_subscriptions.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_folder_subscriptions_dialog();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        action_folder_subscriptions.set_enabled(false);
        self.add_action(&action_folder_subscriptions);

        let action_folder_edit_sort_order = gio::SimpleAction::new("folder-edit-sort-order", None);
        action_folder_edit_sort_order.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_folder_sort_order_dialog();
                } else {
                    let msg = gettext("Mail view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        action_folder_edit_sort_order.set_enabled(false);
        self.add_action(&action_folder_edit_sort_order);

        let action_import = gio::SimpleAction::new("import", None);
        action_import.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::ImportDialog::new();
                dialog.present(Some(&win));
            }
        ));
        self.add_action(&action_import);


        let action_purge_calendar = gio::SimpleAction::new("purge-calendar", None);
        action_purge_calendar.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(cal_page) = win.calendar_page() {
                    cal_page.present_calendar_purge_dialog();
                } else {
                    let msg = gettext("Calendar view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        action_purge_calendar.set_enabled(false);
        self.add_action(&action_purge_calendar);

        let action_export_calendar = gio::SimpleAction::new("export-calendar", None);
        action_export_calendar.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(cal_page) = win.calendar_page() {
                    cal_page.export_calendar_dialog();
                } else {
                    let msg = gettext("Calendar view is unavailable");
                    win.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        action_export_calendar.set_enabled(false);
        self.add_action(&action_export_calendar);

        let action_zoom_in = gio::SimpleAction::new("zoom-in", None);
        action_zoom_in.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if win.active_view_name().as_str() == "calendar" {
                    if let Some(calendar_page) = win.calendar_page() {
                        calendar_page.zoom_in();
                    }
                } else if let Some(mail_page) = win.mail_page() {
                    mail_page.zoom_in();
                }
            }
        ));
        self.add_action(&action_zoom_in);

        let action_zoom_out = gio::SimpleAction::new("zoom-out", None);
        action_zoom_out.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if win.active_view_name().as_str() == "calendar" {
                    if let Some(calendar_page) = win.calendar_page() {
                        calendar_page.zoom_out();
                    }
                } else if let Some(mail_page) = win.mail_page() {
                    mail_page.zoom_out();
                }
            }
        ));
        self.add_action(&action_zoom_out);

        let action_zoom_reset = gio::SimpleAction::new("zoom-reset", None);
        action_zoom_reset.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if win.active_view_name().as_str() == "calendar" {
                    if let Some(calendar_page) = win.calendar_page() {
                        calendar_page.zoom_reset();
                    }
                } else if let Some(mail_page) = win.mail_page() {
                    mail_page.zoom_reset();
                }
            }
        ));
        self.add_action(&action_zoom_reset);

        let action_expand_threads = gio::SimpleAction::new("expand-all-threads", None);
        action_expand_threads.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.expand_all_threads();
                }
            }
        ));
        self.add_action(&action_expand_threads);

        let action_collapse_threads = gio::SimpleAction::new("collapse-all-threads", None);
        action_collapse_threads.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.collapse_all_threads();
                }
            }
        ));
        self.add_action(&action_collapse_threads);

        let action_mark_thread_read = gio::SimpleAction::new("mark-thread-read", None);
        action_mark_thread_read.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    match mail_page.mark_selected_thread_read() {
                        Ok(count) => {
                            if count > 0 {
                                let msg = ngettext(
                                    "{count} conversation message marked as read",
                                    "{count} conversation messages marked as read",
                                    count as u32,
                                )
                                .replace("{count}", &count.to_string());
                                win.add_toast(&adw::Toast::new(&msg));
                            } else {
                                let msg = gettext("No unread messages in conversation");
                                win.add_toast(&adw::Toast::new(&msg));
                            }
                        }
                        Err(error) => {
                            let msg = gettext("Error marking conversation: {error}")
                                .replace("{error}", &error.to_string());
                            win.add_toast(&adw::Toast::new(&msg));
                        }
                    }
                }
            }
        ));
        self.add_action(&action_mark_thread_read);

        let action_delete_thread = gio::SimpleAction::new("delete-thread", None);
        action_delete_thread.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    match mail_page.delete_selected_thread() {
                        Ok(count) => {
                            if count > 0 {
                                let msg = ngettext(
                                    "{count} conversation message deleted",
                                    "{count} conversation messages deleted",
                                    count as u32,
                                )
                                .replace("{count}", &count.to_string());
                                win.add_toast(&adw::Toast::new(&msg));
                            }
                        }
                        Err(error) => {
                            let msg = gettext("Error deleting conversation: {error}")
                                .replace("{error}", &error.to_string());
                            win.add_toast(&adw::Toast::new(&msg));
                        }
                    }
                }
            }
        ));
        self.add_action(&action_delete_thread);

        for (action_name, heading, body, success) in [
            (
                "empty-trash",
                gettext("Empty Trash?"),
                gettext(
                    "Local app-specific messages will be removed from the visible Trash view. Synchronized folders are expunged via the backend path.",
                ),
                gettext("Trash emptied"),
            ),
            (
                "empty-junk",
                gettext("Empty Junk?"),
                gettext(
                    "All local messages in the Junk folder will be irrevocably deleted.",
                ),
                gettext("Junk folder emptied"),
            ),
            (
                "expunge",
                gettext("Permanently empty Trash?"),
                gettext(
                    "The local app-specific Trash will be emptied. Synchronized folders require the backend path.",
                ),
                gettext("Trash permanently emptied"),
            ),
        ] {
            let action = gio::SimpleAction::new(action_name, None);
            let confirm = gettext("Empty");
            action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| {
                    let win_for_action = win.clone();
                    let success = success.clone();
                    let action_key = action_name.to_string();
                    crate::dialogs::run_destructive_action(
                        &win,
                        &heading,
                        &body,
                        &confirm,
                        move || {
                            if let Some(mail_page) = win_for_action.mail_page() {
                                if action_key == "empty-junk" {
                                    mail_page.empty_junk_async(&success);
                                } else {
                                    mail_page.empty_trash_async(&success);
                                }
                            } else {
                                let msg = gettext("Mail view is unavailable");
                                win_for_action.add_toast(&adw::Toast::new(&msg));
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

        let action_search_advanced = gio::SimpleAction::new("search-advanced", None);
        action_search_advanced.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                if let Some(mail_page) = window.mail_page() {
                    mail_page.present_advanced_search_dialog();
                }
            }
        ));
        self.add_action(&action_search_advanced);

        let action_find_in_message = gio::SimpleAction::new("find-in-message", None);
        action_find_in_message.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                let opened = window
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.find_in_displayed_message());
                if !opened {
                    let msg = gettext(
                        "A message must be displayed in the preview pane to search",
                    );
                    window.add_toast(&adw::Toast::new(&msg));
                }
            }
        ));
        self.add_action(&action_find_in_message);

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

        for (action_name, forward) in [("mail-next-unread", true), ("mail-prev-unread", false)] {
            let action = gio::SimpleAction::new(action_name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| {
                    let selected = window
                        .mail_page()
                        .is_some_and(|page| page.select_relative_unread_message(forward));
                    if !selected {
                        let msg = gettext("There are no unread messages in this folder");
                        window.add_toast(&adw::Toast::new(&msg));
                    }
                }
            ));
            self.add_action(&action);
        }

        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            let window = self.downgrade();
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(window) = window.upgrade() {
                    let visible = stack.visible_child_name();
                    window.set_mail_actions_enabled(
                        visible
                            .as_deref()
                            .is_some_and(|view_name| view_name == "mail"),
                    );
                    window.set_calendar_actions_enabled(
                        visible
                            .as_deref()
                            .is_some_and(|view_name| view_name == "calendar"),
                    );
                }
            });
            let visible = view_stack.visible_child_name();
            self.set_mail_actions_enabled(
                visible
                    .as_deref()
                    .is_some_and(|view_name| view_name == "mail"),
            );
            self.set_calendar_actions_enabled(
                visible
                    .as_deref()
                    .is_some_and(|view_name| view_name == "calendar"),
            );
        }

        if let Some(mail_page) = self.mail_page() {
            let window = self.downgrade();
            mail_page.connect_selected_message_notify(move |_has_selection| {
                if let Some(window) = window.upgrade() {
                    let is_mail_view = window
                        .imp()
                        .view_stack
                        .borrow()
                        .as_ref()
                        .and_then(|view_stack| view_stack.visible_child_name())
                        .is_some_and(|view_name| view_name == "mail");
                    window.set_message_actions_enabled(is_mail_view);
                    window.update_mail_navigation_actions(is_mail_view);
                    window.update_mail_undelete_action(is_mail_view);
                }
            });
        }
    }
}

pub(crate) fn format_current_status_date() -> String {
    use chrono::{Datelike, Local, Weekday};
    let now = Local::now();
    let weekday = match now.weekday() {
        Weekday::Mon => crate::i18n::gettext("Monday"),
        Weekday::Tue => crate::i18n::gettext("Tuesday"),
        Weekday::Wed => crate::i18n::gettext("Wednesday"),
        Weekday::Thu => crate::i18n::gettext("Thursday"),
        Weekday::Fri => crate::i18n::gettext("Friday"),
        Weekday::Sat => crate::i18n::gettext("Saturday"),
        Weekday::Sun => crate::i18n::gettext("Sunday"),
    };
    let month = match now.month() {
        1 => crate::i18n::gettext("January"),
        2 => crate::i18n::gettext("February"),
        3 => crate::i18n::gettext("March"),
        4 => crate::i18n::gettext("April"),
        5 => crate::i18n::gettext("May"),
        6 => crate::i18n::gettext("June"),
        7 => crate::i18n::gettext("July"),
        8 => crate::i18n::gettext("August"),
        9 => crate::i18n::gettext("September"),
        10 => crate::i18n::gettext("October"),
        11 => crate::i18n::gettext("November"),
        12 => crate::i18n::gettext("December"),
        _ => String::new(),
    };
    format!("{}, {} {} {}", weekday, now.day(), month, now.year())
}

fn build_debug_console_panel(revealer: &gtk::Revealer) -> gtk::Widget {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(false)
        .valign(gtk::Align::End)
        .css_classes(vec!["debug-console-panel"])
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(4)
        .margin_bottom(4)
        .css_classes(vec!["debug-console-header"])
        .build();

    let title_icon = gtk::Image::from_icon_name("utilities-terminal-symbolic");
    let title_label_text = gettext("Log Console");
    let title_label = gtk::Label::builder()
        .label(&title_label_text)
        .css_classes(vec!["heading"])
        .halign(gtk::Align::Start)
        .build();

    let search_placeholder = gettext("Filter logs…");
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(&search_placeholder)
        .hexpand(true)
        .margin_start(16)
        .margin_end(16)
        .build();

    let pause_tooltip = gettext("Pause live updates");
    let pause_button = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text(&pause_tooltip)
        .css_classes(vec!["flat"])
        .build();

    let copy_tooltip = gettext("Copy log to clipboard");
    let copy_button = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(&copy_tooltip)
        .css_classes(vec!["flat"])
        .build();

    let clear_tooltip = gettext("Clear log");
    let clear_button = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(&clear_tooltip)
        .css_classes(vec!["flat"])
        .build();

    let collapse_tooltip = gettext("Collapse log console");
    let collapse_button = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(&collapse_tooltip)
        .css_classes(vec!["flat"])
        .build();

    let close_tooltip = gettext("Close log console");
    let close_button = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(&close_tooltip)
        .css_classes(vec!["flat"])
        .build();

    header.append(&title_icon);
    header.append(&title_label);
    header.append(&search_entry);
    header.append(&pause_button);
    header.append(&copy_button);
    header.append(&clear_button);
    header.append(&collapse_button);
    header.append(&close_button);

    let text_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(vec!["monospace", "terminal-text"])
        .margin_start(8)
        .margin_end(8)
        .margin_top(4)
        .margin_bottom(4)
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .min_content_height(120)
        .max_content_height(180)
        .vexpand(false)
        .valign(gtk::Align::End)
        .child(&text_view)
        .build();

    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    container.append(&sep);
    container.append(&header);
    container.append(&scrolled);

    let revealer_for_close = revealer.clone();
    let do_close = Rc::new(move || {
        revealer_for_close.set_reveal_child(false);
        if let Some(settings) = AppSettings::try_new() {
            settings.set_debug_console_enabled(false);
        }
    });
    let do_close_cloned = Rc::clone(&do_close);
    collapse_button.connect_clicked(move |_| {
        do_close();
    });
    close_button.connect_clicked(move |_| {
        do_close_cloned();
    });

    let buffer = text_view.buffer();
    let search_filter = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let search_for_entry = std::rc::Rc::clone(&search_filter);
    let buffer_for_entry = buffer.clone();
    let text_view_for_entry = text_view.clone();

    let refresh_logs = std::rc::Rc::new(
        move |buf: &gtk::TextBuffer, tv: &gtk::TextView, filter: &str| {
            let logs = crate::services::log_buffer::recent_logs();
            let filter_lower = filter.trim().to_lowercase();
            let mut filtered_text: String = logs
                .iter()
                .filter(|line| filter_lower.is_empty() || line.to_lowercase().contains(&filter_lower))
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            if filtered_text.contains('\0') {
                filtered_text = filtered_text.replace('\0', "");
            }
            buf.set_text(&filtered_text);

            let mark = buf.create_mark(None, &buf.end_iter(), false);
            tv.scroll_to_mark(&mark, 0.0, true, 0.0, 1.0);
            buf.delete_mark(&mark);
        },
    );

    let refresh_for_search = std::rc::Rc::clone(&refresh_logs);
    search_entry.connect_search_changed(move |entry| {
        let text = entry.text().to_string();
        *search_for_entry.borrow_mut() = text.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            refresh_for_search(&buffer_for_entry, &text_view_for_entry, &text);
        }));
    });

    let buffer_for_clear = buffer.clone();
    clear_button.connect_clicked(move |_| {
        crate::services::log_buffer::clear_logs();
        buffer_for_clear.set_text("");
    });

    let buffer_for_copy = buffer.clone();
    copy_button.connect_clicked(move |btn| {
        let text = buffer_for_copy.text(
            &buffer_for_copy.start_iter(),
            &buffer_for_copy.end_iter(),
            false,
        );
        let clean_text = if text.contains('\0') {
            text.replace('\0', "")
        } else {
            text.to_string()
        };
        btn.clipboard().set_text(&clean_text);
    });

    let buffer_for_notify = buffer.clone();
    let text_view_for_notify = text_view.clone();
    let search_for_notify = std::rc::Rc::clone(&search_filter);
    let refresh_for_notify = std::rc::Rc::clone(&refresh_logs);
    revealer.connect_reveal_child_notify(move |r| {
        if r.reveals_child() {
            let filter = search_for_notify.borrow().clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                refresh_for_notify(&buffer_for_notify, &text_view_for_notify, &filter);
            }));
        }
    });

    let is_paused = std::rc::Rc::new(std::cell::Cell::new(false));
    let is_paused_for_btn = std::rc::Rc::clone(&is_paused);
    let refresh_for_pause = std::rc::Rc::clone(&refresh_logs);
    let buffer_for_pause = buffer.clone();
    let text_view_for_pause = text_view.clone();
    let search_for_pause = std::rc::Rc::clone(&search_filter);

    pause_button.connect_toggled(move |btn| {
        let paused = btn.is_active();
        is_paused_for_btn.set(paused);
        if paused {
            btn.set_icon_name("media-playback-start-symbolic");
            btn.set_tooltip_text(Some("Live-Aktualisierung fortsetzen"));
        } else {
            btn.set_icon_name("media-playback-pause-symbolic");
            btn.set_tooltip_text(Some("Live-Aktualisierung anhalten (Pause)"));
            let filter = search_for_pause.borrow().clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                refresh_for_pause(&buffer_for_pause, &text_view_for_pause, &filter);
            }));
        }
    });

    let revealer_for_timer = revealer.clone();
    let buffer_for_timer = buffer.clone();
    let text_view_for_timer = text_view.clone();
    let search_for_timer = std::rc::Rc::clone(&search_filter);
    let refresh_for_timer = std::rc::Rc::clone(&refresh_logs);
    let is_paused_for_timer = std::rc::Rc::clone(&is_paused);
    glib::timeout_add_seconds_local(1, move || {
        if !is_paused_for_timer.get()
            && revealer_for_timer.reveals_child()
            && revealer_for_timer.is_visible()
        {
            let filter = search_for_timer.borrow().clone();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                refresh_for_timer(&buffer_for_timer, &text_view_for_timer, &filter);
            }));
        }
        glib::ControlFlow::Continue
    });

    container.upcast()
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::action_contract::{ACTIONS, menu_actions};

    fn add_test_page(stack: &adw::ViewStack, name: &str) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.add_named(&page, Some(name));
        page
    }

    pub(crate) fn drain_main_context(context: &glib::MainContext) {
        for _ in 0..32 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
    }

    fn restore_visible_page(stack: &adw::ViewStack, visible_page: Option<&str>) {
        if let Some(visible_page) = visible_page {
            stack.set_visible_child_name(visible_page);
        }
    }

    #[test]
    fn window_action_contract_is_registered() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        for spec in ACTIONS
            .iter()
            .filter(|spec| spec.action.starts_with("win."))
        {
            let name = spec.action.trim_start_matches("win.");
            assert!(
                window.lookup_action(name).is_some(),
                "missing window action: {name}"
            );
        }
    }

    #[test]
    fn null_message_selection_disables_message_scope_but_keeps_shell_actions() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        window.set_message_actions_enabled(false);
        for spec in ACTIONS
            .iter()
            .filter(|spec| spec.scope == ActionScope::MessageSelection)
        {
            let name = spec.action.trim_start_matches("win.");
            let action = window
                .lookup_action(name)
                .expect("action must be registered");
            assert!(
                !action.is_enabled(),
                "message action stayed enabled: {name}"
            );
        }
        for name in ["search", "switch-mail", "switch-calendar", "new"] {
            let action = window.lookup_action(name).expect("shell action must exist");
            assert!(action.is_enabled(), "shell action was disabled: {name}");
        }
    }

    #[test]
    fn switcher_location_action_toggles_and_syncs_setting() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let Some(settings) = AppSettings::try_new() else {
            return;
        };
        let original_location = settings.switcher_location();

        // Ensure clean starting state
        settings.set_switcher_location("bottom-left");

        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let header_bar = window.imp().header_bar.borrow().as_ref().unwrap().clone();
        assert!(header_bar.title_widget().unwrap().downcast_ref::<adw::WindowTitle>().is_some());

        let action = window
            .lookup_action("toggle-top-switcher")
            .expect("missing toggle-top-switcher action");

        let state: bool = action.state().unwrap().get().unwrap();
        assert!(!state);

        // Activate action -> should flip to top
        action.activate(None);
        drain_main_context(&context);
        assert_eq!(settings.switcher_location(), "top");
        let state: bool = action.state().unwrap().get().unwrap();
        assert!(state);
        assert!(header_bar.title_widget().unwrap().downcast_ref::<adw::ViewSwitcher>().is_some());

        // Activate action -> should flip back to bottom-left
        action.activate(None);
        drain_main_context(&context);
        assert_eq!(settings.switcher_location(), "bottom-left");
        let state: bool = action.state().unwrap().get().unwrap();
        assert!(!state);
        assert!(header_bar.title_widget().unwrap().downcast_ref::<adw::WindowTitle>().is_some());

        settings.set_switcher_location(&original_location);
    }

    fn wait_for_search_route_source_to_clear(
        context: &glib::MainContext,
        window: &MeridianWindow,
        max_wait: std::time::Duration,
    ) {
        let deadline = Instant::now() + max_wait;
        while window.imp().search_route_source.borrow().is_some() {
            assert!(
                Instant::now() < deadline,
                "search route source did not terminate within the test bound"
            );
            if context.pending() {
                context.iteration(false);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn search_target_routes_same_uid_by_component_kind_without_parsing_identity() {
        let source_uid = "source:with&separator".to_string();
        let component_uid = "uid:with&separator".to_string();
        let event = SearchTarget::Event {
            source_uid: source_uid.clone(),
            component_uid: component_uid.clone(),
        };
        let task = SearchTarget::Task {
            source_uid: source_uid.clone(),
            component_uid: component_uid.clone(),
        };
        let memo = SearchTarget::Memo {
            source_uid,
            component_uid,
        };

        assert_eq!(
            search_route_for_target(Some(&event)),
            Ok(SearchRoute::Calendar)
        );
        assert_eq!(search_route_for_target(Some(&task)), Ok(SearchRoute::Tasks));
        assert_eq!(search_route_for_target(Some(&memo)), Ok(SearchRoute::Memos));
        assert_ne!(event, task);
        assert_ne!(task, memo);
    }

    #[test]
    fn search_target_preserves_contact_source_and_valid_empty_source() {
        let mail = SearchTarget::Mail {
            local_id: "local:Inbox&copy".to_string(),
        };
        let contact = SearchTarget::Contact {
            source_uid: "addressbook:primary&shared".to_string(),
            contact_uid: "contact:1&2".to_string(),
        };
        let empty_source_event = SearchTarget::Event {
            source_uid: String::new(),
            component_uid: "external-event".to_string(),
        };

        assert_eq!(search_route_for_target(Some(&mail)), Ok(SearchRoute::Mail));
        assert_eq!(
            search_route_for_target(Some(&contact)),
            Ok(SearchRoute::Contacts)
        );
        assert_eq!(
            search_route_for_target(Some(&empty_source_event)),
            Ok(SearchRoute::Calendar)
        );
        assert_eq!(
            contact,
            SearchTarget::Contact {
                source_uid: "addressbook:primary&shared".to_string(),
                contact_uid: "contact:1&2".to_string(),
            }
        );
    }

    #[test]
    fn missing_and_malformed_search_targets_are_explicitly_rejected() {
        assert_eq!(
            search_route_for_target(None),
            Err(SearchRouteError::MissingTarget)
        );
        assert_eq!(
            search_route_for_target(Some(&SearchTarget::Mail {
                local_id: String::new(),
            })),
            Err(SearchRouteError::EmptyMailId)
        );
        assert_eq!(
            search_route_for_target(Some(&SearchTarget::Task {
                source_uid: String::new(),
                component_uid: String::new(),
            })),
            Err(SearchRouteError::EmptyComponentUid)
        );
        assert_eq!(
            search_route_for_target(Some(&SearchTarget::Contact {
                source_uid: "addressbook".to_string(),
                contact_uid: String::new(),
            })),
            Err(SearchRouteError::EmptyContactUid)
        );
        assert_eq!(
            search_route_error_message(SearchRouteError::MissingTarget),
            crate::i18n::gettext("Global search match has no target")
        );
    }

    #[test]
    fn stale_or_superseded_search_routes_cannot_apply_or_retry() {
        let first = next_search_route_generation(11);
        let second = next_search_route_generation(first);
        assert_eq!(first, 12);
        assert_eq!(second, 13);
        assert_ne!(first, second);

        let state = SearchRouteState {
            generation: second,
            destination: Some(SearchRoute::Tasks.page_name()),
        };
        assert_eq!(
            search_route_decision(
                state,
                first,
                SearchRoute::Calendar,
                Some("calendar"),
                1,
                true,
            ),
            SearchRouteDecision::RejectStale
        );
        let state = SearchRouteState {
            generation: first,
            destination: Some(SearchRoute::Calendar.page_name()),
        };
        assert_eq!(
            search_route_decision(
                state,
                first,
                SearchRoute::Calendar,
                Some("calendar"),
                1,
                true,
            ),
            SearchRouteDecision::Complete
        );
        assert_eq!(
            search_route_decision(
                state,
                first,
                SearchRoute::Calendar,
                Some("calendar"),
                1,
                false,
            ),
            SearchRouteDecision::Retry
        );
        assert_eq!(
            search_route_decision(
                state,
                first,
                SearchRoute::Calendar,
                Some("calendar"),
                MAX_SEARCH_ROUTE_ATTEMPTS,
                false,
            ),
            SearchRouteDecision::Failed
        );
    }

    #[test]
    fn navigation_away_invalidates_pending_search_route_state() {
        let mut state = SearchRouteState::default();
        let generation = state.begin(SearchRoute::Calendar);

        assert!(!state.invalidate_for_visible_page(Some("calendar")));
        assert!(state.invalidate_for_visible_page(Some("mail")));
        assert_eq!(state.destination, None);
        assert_ne!(state.generation, generation);
        assert_eq!(
            search_route_decision(
                state,
                generation,
                SearchRoute::Calendar,
                Some("calendar"),
                1,
                false,
            ),
            SearchRouteDecision::RejectStale
        );
    }

    #[test]
    fn stale_timer_after_visible_child_change_cannot_retry_or_open() {
        let mut state = SearchRouteState::default();
        let generation = state.begin(SearchRoute::Contacts);

        assert_eq!(
            search_route_decision(
                state,
                generation,
                SearchRoute::Contacts,
                Some("contacts"),
                1,
                false,
            ),
            SearchRouteDecision::Retry
        );
        assert_eq!(
            search_route_decision(
                state,
                generation,
                SearchRoute::Contacts,
                Some("mail"),
                1,
                false,
            ),
            SearchRouteDecision::RejectStale
        );

        state.invalidate_for_visible_page(Some("mail"));
        assert_eq!(
            search_route_decision(
                state,
                generation,
                SearchRoute::Contacts,
                Some("contacts"),
                2,
                true,
            ),
            SearchRouteDecision::RejectStale
        );
    }

    #[test]
    fn gtk_visible_page_change_invalidates_and_removes_pending_search_route_source() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("window construction must install its view stack");
        let initial_page = stack.visible_child_name().map(|name| name.to_string());
        let _test_page = add_test_page(&stack, "gtk-test-hidden");

        window.show_search_result(Some(SearchTarget::Mail {
            local_id: "gtk-test-search-route-never-matches".to_string(),
        }));
        assert!(window.imp().search_route_source.borrow().is_some());
        assert_eq!(
            window.imp().search_route_state.get().destination,
            Some(SearchRoute::Mail.page_name())
        );

        stack.set_visible_child_name("gtk-test-hidden");
        drain_main_context(&context);

        assert!(window.imp().search_route_source.borrow().is_none());
        assert_eq!(window.imp().search_route_state.get().destination, None);
        assert_eq!(
            stack.visible_child_name().as_deref(),
            Some("gtk-test-hidden")
        );

        restore_visible_page(&stack, initial_page.as_deref());
        drain_main_context(&context);
    }

    #[test]
    fn calendar_purge_action_is_enabled_only_for_the_calendar_page() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("window construction must install its view stack");
        let initial_page = stack.visible_child_name().map(|name| name.to_string());
        let action = window
            .lookup_action("purge-calendar")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
            .expect("calendar purge action must be registered");

        stack.set_visible_child_name("calendar");
        drain_main_context(&context);
        assert!(action.is_enabled());

        stack.set_visible_child_name("mail");
        drain_main_context(&context);
        assert!(!action.is_enabled());

        restore_visible_page(&stack, initial_page.as_deref());
        drain_main_context(&context);
    }

    #[test]
    fn gtk_hidden_page_callback_rejects_before_selecting_or_opening() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("window construction must install its view stack");
        let initial_page = stack.visible_child_name().map(|name| name.to_string());
        let _test_page = add_test_page(&stack, "gtk-test-hidden");

        window.show_search_result(Some(SearchTarget::Mail {
            local_id: "gtk-test-hidden-route-never-matches".to_string(),
        }));
        assert!(window.imp().search_route_source.borrow().is_some());

        let notifications = stack.freeze_notify();
        stack.set_visible_child_name("gtk-test-hidden");
        assert_eq!(
            stack.visible_child_name().as_deref(),
            Some("gtk-test-hidden")
        );

        // Keep the visible-child notification queued so the real timeout
        // callback must reject the hidden page before calling the page API.
        wait_for_search_route_source_to_clear(
            &context,
            &window,
            std::time::Duration::from_millis(500),
        );
        assert_eq!(window.imp().search_route_state.get().destination, None);
        drop(notifications);

        restore_visible_page(&stack, initial_page.as_deref());
        drain_main_context(&context);
    }

    #[test]
    fn gtk_search_route_callback_terminates_after_bounded_attempts() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("window construction must install its view stack");
        let initial_page = stack.visible_child_name().map(|name| name.to_string());

        window.show_search_result(Some(SearchTarget::Mail {
            local_id: "gtk-test-bounded-route-never-matches".to_string(),
        }));
        assert!(window.imp().search_route_source.borrow().is_some());

        wait_for_search_route_source_to_clear(&context, &window, std::time::Duration::from_secs(3));
        assert_eq!(window.imp().search_route_state.get().destination, None);

        restore_visible_page(&stack, initial_page.as_deref());
        drain_main_context(&context);
    }

    #[test]
    fn search_routing_production_has_no_lossy_identity_or_calendar_scan() {
        let source = include_str!("window.rs");
        let production = source
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(source);
        assert!(production.contains("fn open_search_target(&self, route: SearchRoute"));
        assert!(production.contains("SearchTarget::Event"));
        assert!(production.contains("SearchTarget::Task"));
        assert!(production.contains("SearchTarget::Memo"));
        assert!(production.contains("SearchTarget::Contact"));
    }

    #[test]
    fn undelete_recovery_is_exposed_without_aliasing_generic_undo() {
        let source = include_str!("window.rs");
        for path in [MenuPath::Edit, MenuPath::MessageAfterForward] {
            assert!(menu_actions(path).any(|item| {
                item.label == "Undelete" && item.action == "win.undelete"
            }));
        }
        assert!(
            source.contains("let action_undelete = gio::SimpleAction::new(\"undelete\", None);")
        );
        assert!(source.contains("mail_page.undelete()"));
        assert!(source.contains("mail_page.can_undelete()"));
        assert!(source.contains("win.toggle-mail-show-deleted"));
        assert!(source.contains("mail-show-deleted"));
    }

    #[test]
    fn folder_sort_order_action_is_in_the_folder_menu_and_uses_page_sensitivity() {
        let source = include_str!("window.rs");
        assert!(
            menu_actions(MenuPath::Folder).any(|item| item.action == "win.folder-edit-sort-order")
        );
        assert!(source.contains(
            "let action_folder_edit_sort_order = gio::SimpleAction::new(\"folder-edit-sort-order\", None);"
        ));
        assert!(source.contains("mail_page.can_edit_folder_sort_order()"));
        assert!(source.contains("mail_page.present_folder_sort_order_dialog()"));
    }

    #[test]
    fn folder_properties_and_native_subscriptions_are_independently_gated() {
        let source = include_str!("window.rs");
        assert!(source.contains("mail_page.can_present_folder_properties()"));
        assert!(
            source.contains(
                "root.set_folder_properties_action_enabled(target_has_properties_snapshot)"
            )
        );
        assert!(source.contains("action_folder_subscriptions.set_enabled(false)"));
        assert!(source.contains("mail_page.can_present_folder_subscriptions()"));
        assert!(source.contains("mail_page.present_folder_subscriptions_dialog()"));
    }

    #[test]
    fn advanced_search_action_delegates_to_the_mail_page() {
        let source = include_str!("window.rs");
        assert!(source.contains(
            "let action_search_advanced = gio::SimpleAction::new(\"search-advanced\", None);"
        ));
        assert!(source.contains("mail_page.present_advanced_search_dialog()"));
    }

    #[test]
    fn sidebar_position_syncs_across_views_and_persists() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("view stack present");

        let mail_page = stack
            .child_by_name("mail")
            .and_downcast::<MailPage>()
            .expect("mail page");

        let initial_pos = mail_page.outer_paned().position();
        assert!(initial_pos >= 100 && initial_pos <= 500);

        // Switch to calendar
        window.set_active_view("calendar");
        let calendar_bin = stack
            .child_by_name("calendar")
            .and_downcast::<adw::Bin>()
            .expect("calendar bin");
        let calendar_page = calendar_bin
            .child()
            .and_downcast::<CalendarPage>()
            .expect("calendar page");
        let calendar_paned = calendar_page.outer_paned().expect("calendar paned");
        assert_eq!(calendar_paned.position(), initial_pos);

        // Resize sidebar in calendar page
        calendar_paned.set_position(330);
        if let Some(settings) = AppSettings::try_new() {
            settings.set_mail_sidebar_position(330);
            assert_eq!(settings.mail_sidebar_position(), 330);
        }

        // Switch to tasks
        window.set_active_view("tasks");
        let tasks_bin = stack
            .child_by_name("tasks")
            .and_downcast::<adw::Bin>()
            .expect("tasks bin");
        let tasks_page = tasks_bin
            .child()
            .and_downcast::<TasksPage>()
            .expect("tasks page");
        let tasks_paned = tasks_page.outer_paned().expect("tasks paned");
        assert_eq!(tasks_paned.position(), 330);

        // Switch to memos
        window.set_active_view("memos");
        let memos_bin = stack
            .child_by_name("memos")
            .and_downcast::<adw::Bin>()
            .expect("memos bin");
        let memos_page = memos_bin
            .child()
            .and_downcast::<MemosPage>()
            .expect("memos page");
        let memos_paned = memos_page.outer_paned().expect("memos paned");
        assert_eq!(memos_paned.position(), 330);

        // Switch to contacts - contacts divider uses the unified sidebar position (330)
        window.set_active_view("contacts");
        let contacts_bin = stack
            .child_by_name("contacts")
            .and_downcast::<adw::Bin>()
            .expect("contacts bin");
        let contacts_page = contacts_bin
            .child()
            .and_downcast::<ContactsPage>()
            .expect("contacts page");
        let contacts_paned = contacts_page.outer_paned().expect("contacts paned");
        assert_eq!(contacts_paned.position(), 330);

        // Switch back to mail
        window.set_active_view("mail");
        assert_eq!(mail_page.outer_paned().position(), 330);
    }

    #[test]
    fn test_unified_sidebar_width_clamping_and_cross_view_synchronization() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view("mail");
            settings.set_mail_show_sidebar(true);
            settings.set_mail_sidebar_position(260);
        }
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        // 1. Verify bounds constants
        assert_eq!(MIN_SIDEBAR_WIDTH, 240);
        assert_eq!(MAX_SIDEBAR_WIDTH, 450);

        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("view stack present");

        let mail_page = stack
            .child_by_name("mail")
            .and_downcast::<MailPage>()
            .expect("mail page");

        let sb_h = window
            .imp()
            .sidebar_header
            .borrow()
            .clone()
            .expect("sidebar_header");

        // 2. Synchronize clamped position
        if let Some(settings) = AppSettings::try_new() {
            // Setting position to 500 should be clamped to MAX_SIDEBAR_WIDTH (450)
            settings.set_mail_sidebar_position(500);
            assert_eq!(mail_page.outer_paned().position(), MAX_SIDEBAR_WIDTH);
            assert_eq!(sb_h.size_request().0, MAX_SIDEBAR_WIDTH);

            // Setting position to 120 should be clamped to MIN_SIDEBAR_WIDTH (240)
            settings.set_mail_sidebar_position(120);
            assert_eq!(mail_page.outer_paned().position(), MIN_SIDEBAR_WIDTH);
            assert_eq!(sb_h.size_request().0, MIN_SIDEBAR_WIDTH);

            // Setting position to valid 300
            settings.set_mail_sidebar_position(300);
            assert_eq!(mail_page.outer_paned().position(), 300);
            assert_eq!(sb_h.size_request().0, 300);
        }

        // 3. Switch to calendar and check width and header sync
        window.set_active_view("calendar");
        let calendar_bin = stack
            .child_by_name("calendar")
            .and_downcast::<adw::Bin>()
            .expect("calendar bin");
        let calendar_page = calendar_bin
            .child()
            .and_downcast::<CalendarPage>()
            .expect("calendar page");
        let calendar_paned = calendar_page.outer_paned().expect("calendar paned");
        assert_eq!(calendar_paned.position(), 300);
        assert_eq!(sb_h.size_request().0, 300);

        // 4. Unified collapse/expand
        window.toggle_sidebar_animated(false);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(!window.is_sidebar_visible_for_active_view());
        assert!(!calendar_page.is_sidebar_visible());
        assert!(!mail_page.is_sidebar_visible());

        // Switch to tasks while collapsed
        window.set_active_view("tasks");
        let tasks_bin = stack
            .child_by_name("tasks")
            .and_downcast::<adw::Bin>()
            .expect("tasks bin");
        let tasks_page = tasks_bin
            .child()
            .and_downcast::<TasksPage>()
            .expect("tasks page");
        assert!(!window.is_sidebar_visible_for_active_view());
        assert!(!tasks_page.is_sidebar_visible());

        // Expand sidebar
        window.toggle_sidebar_animated(true);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(window.is_sidebar_visible_for_active_view());
        assert!(tasks_page.is_sidebar_visible());
        assert!(calendar_page.is_sidebar_visible());
        assert!(mail_page.is_sidebar_visible());

        // 5. Contacts sidebar unified cross-view synchronization
        window.set_active_view("contacts");
        let contacts_bin = stack
            .child_by_name("contacts")
            .and_downcast::<adw::Bin>()
            .expect("contacts bin");
        let contacts_page = contacts_bin
            .child()
            .and_downcast::<ContactsPage>()
            .expect("contacts page");
        let contacts_paned = contacts_page.outer_paned().expect("contacts paned");
        assert_eq!(contacts_paned.position(), 300);
        assert_eq!(sb_h.size_request().0, 300);

        // Switch back to mail: mail remains at 300
        window.set_active_view("mail");
        assert_eq!(mail_page.outer_paned().position(), 300);
        assert_eq!(sb_h.size_request().0, 300);
    }

    #[test]
    fn execute_exit_purge_and_expunge_runs_when_configured() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        // Verify execute_exit_purge_and_expunge executes safely
        window.execute_exit_purge_and_expunge();
    }

    #[test]
    fn toolbar_view_content_is_view_stack_and_debug_revealer_is_bottom_bar() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("view stack present");

        // Ensure view stack is set to expand in both directions and fill
        assert!(stack.vexpands());
        assert!(stack.hexpands());
        assert_eq!(stack.valign(), gtk::Align::Fill);
        assert_eq!(stack.halign(), gtk::Align::Fill);

        // Ensure debug revealer is present and does not vexpand
        let revealer = window
            .imp()
            .debug_revealer
            .borrow()
            .clone()
            .expect("debug revealer should exist");
        assert!(!revealer.vexpands());

        // Toggle debug console open and closed
        window.set_debug_console_open(true);
        assert!(revealer.reveals_child());
        assert!(stack.vexpands());
        drain_main_context(&_context);

        window.toggle_debug_console();
        drain_main_context(&_context);

        window.set_debug_console_open(false);
        assert!(!revealer.reveals_child());
        assert!(stack.vexpands());
        drain_main_context(&_context);
    }

    #[test]
    fn debug_console_opens_safely_even_with_nul_bytes_in_log_buffer() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };

        // Inject log entries containing interior nul bytes
        crate::services::log_buffer::clear_logs();
        crate::services::log_buffer::append_log_line("Test line with \0 interior null characters \0 in log");

        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        // Toggle console open - this exercises connect_reveal_child_notify, refresh_logs,
        // TextBuffer::set_text, and scrolling without panic
        window.set_debug_console_open(true);
        drain_main_context(&context);

        window.toggle_debug_console();
        drain_main_context(&context);

        window.set_debug_console_open(false);
        drain_main_context(&context);

        crate::services::log_buffer::clear_logs();
    }

    #[test]
    fn toolbar_toggle_action_is_bidirectional_and_syncs_across_views() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let action = window
            .lookup_action("toggle-toolbar")
            .expect("toggle-toolbar action should be registered");

        let initial_state = action
            .state()
            .and_then(|v| v.get::<bool>())
            .expect("boolean state");

        // Action must support change_state (which GtkPopoverMenu uses)
        action.change_state(&(!initial_state).to_variant());
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(!initial_state)
        );

        // And toggling again must restore the state (bidirectional)
        action.change_state(&initial_state.to_variant());
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(initial_state)
        );

        // Also test connect_activate
        action.activate(None);
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(!initial_state)
        );
        action.activate(None);
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(initial_state)
        );

        // Verify all 5 views have toolbars and reflect settings
        let stack = window
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .cloned()
            .expect("view stack present");

        let mail_page = stack
            .child_by_name("mail")
            .and_downcast::<MailPage>()
            .expect("mail page");
        assert!(mail_page.imp().classic_toolbar.borrow().is_some());

        window.set_active_view("contacts");
        let contacts_bin = stack.child_by_name("contacts").and_downcast::<adw::Bin>().unwrap();
        let contacts_page = contacts_bin.child().and_downcast::<ContactsPage>().unwrap();
        assert!(contacts_page.imp().toolbar.borrow().is_some());

        window.set_active_view("calendar");
        let calendar_bin = stack.child_by_name("calendar").and_downcast::<adw::Bin>().unwrap();
        let calendar_page = calendar_bin.child().and_downcast::<CalendarPage>().unwrap();
        assert!(calendar_page.imp().toolbar.borrow().is_some());

        window.set_active_view("tasks");
        let tasks_bin = stack.child_by_name("tasks").and_downcast::<adw::Bin>().unwrap();
        let tasks_page = tasks_bin.child().and_downcast::<TasksPage>().unwrap();
        assert!(tasks_page.imp().toolbar.borrow().is_some());

        window.set_active_view("memos");
        let memos_bin = stack.child_by_name("memos").and_downcast::<adw::Bin>().unwrap();
        let memos_page = memos_bin.child().and_downcast::<MemosPage>().unwrap();
        assert!(memos_page.imp().toolbar.borrow().is_some());

        window.set_active_view("mail");

        // Verify all 5 views have toolbars and reflect default visibility (non-calendar hidden, calendar visible)
        assert_eq!(mail_page.imp().classic_toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
        assert_eq!(contacts_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
        assert_eq!(calendar_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
        assert_eq!(tasks_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
        assert_eq!(memos_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));

        // Verify centralized toggle activates all non-calendar views simultaneously while preserving calendar
        window.set_all_views_toolbar_visible(true);
        assert_eq!(mail_page.imp().classic_toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
        assert_eq!(contacts_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
        assert_eq!(calendar_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
        assert_eq!(tasks_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
        assert_eq!(memos_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));

        if let Some(settings) = AppSettings::try_new() {
            // Toggling contacts toolbar individually only affects contacts
            settings.set_contact_show_toolbar(false);
            assert_eq!(contacts_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
            assert_eq!(tasks_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));

            // Toggling tasks toolbar only affects tasks
            settings.set_task_show_toolbar(false);
            assert_eq!(tasks_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
            assert_eq!(memos_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));

            // Toggling memos toolbar only affects memos
            settings.set_memo_show_toolbar(false);
            assert_eq!(memos_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));

            // Restore all to hidden (default)
            window.set_all_views_toolbar_visible(false);
            assert_eq!(mail_page.imp().classic_toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
            assert_eq!(contacts_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
            assert_eq!(calendar_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(true));
            assert_eq!(tasks_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
            assert_eq!(memos_page.imp().toolbar.borrow().as_ref().map(|w| w.is_visible()), Some(false));
        }
    }

    #[test]
    fn statusbar_toggle_is_bidirectional_and_collapses_bottom_bar() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let action = window
            .lookup_action("toggle-statusbar")
            .expect("toggle-statusbar action should be registered");

        let initial_state = action
            .state()
            .and_then(|v| v.get::<bool>())
            .expect("boolean state");

        // Statusbar box should be stored and match initial state
        assert_eq!(window.statusbar_is_visible(), initial_state);

        // When debug console is closed, debug_revealer must be invisible so toolbar view can collapse bottom bar
        let debug_revealer = window
            .imp()
            .debug_revealer
            .borrow()
            .as_ref()
            .cloned()
            .expect("debug revealer exists");
        assert!(!debug_revealer.is_visible(), "debug revealer should be invisible when console is closed");

        // Action must support change_state (used by GtkPopoverMenu)
        action.change_state(&(!initial_state).to_variant());
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(!initial_state)
        );
        assert_eq!(window.statusbar_is_visible(), !initial_state);

        // Toggling again must restore the state (bidirectional)
        action.change_state(&initial_state.to_variant());
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(initial_state)
        );
        assert_eq!(window.statusbar_is_visible(), initial_state);

        // Also test connect_activate
        action.activate(None);
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(!initial_state)
        );
        assert_eq!(window.statusbar_is_visible(), !initial_state);

        action.activate(None);
        assert_eq!(
            action.state().and_then(|v| v.get::<bool>()),
            Some(initial_state)
        );
        assert_eq!(window.statusbar_is_visible(), initial_state);
    }

    #[test]
    fn mail_layout_toggle_is_bidirectional_and_updates_settings() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let classic_action = window
            .lookup_action("use-classic-mail-layout")
            .expect("use-classic-mail-layout action should be registered");
        let modern_action = window
            .lookup_action("use-modern-mail-layout")
            .expect("use-modern-mail-layout action should be registered");

        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Switch to classic/horizontal
        classic_action.change_state(&true.to_variant());
        assert_eq!(settings.mail_layout(), "horizontal");
        assert_eq!(classic_action.state().and_then(|v| v.get::<bool>()), Some(true));
        assert_eq!(modern_action.state().and_then(|v| v.get::<bool>()), Some(false));

        // Switch to modern/vertical via classic toggle (setting false)
        classic_action.change_state(&false.to_variant());
        assert_eq!(settings.mail_layout(), "vertical");
        assert_eq!(classic_action.state().and_then(|v| v.get::<bool>()), Some(false));
        assert_eq!(modern_action.state().and_then(|v| v.get::<bool>()), Some(true));

        // Activate classic again via click (activate)
        classic_action.activate(None);
        assert_eq!(settings.mail_layout(), "horizontal");
        assert_eq!(classic_action.state().and_then(|v| v.get::<bool>()), Some(true));
        assert_eq!(modern_action.state().and_then(|v| v.get::<bool>()), Some(false));

        // Modern action activation also works
        modern_action.activate(None);
        assert_eq!(settings.mail_layout(), "vertical");
        assert_eq!(classic_action.state().and_then(|v| v.get::<bool>()), Some(false));
        assert_eq!(modern_action.state().and_then(|v| v.get::<bool>()), Some(true));
    }

    #[test]
    fn stress_test_window_drain_transport_queue_and_actions() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let Some(settings) = AppSettings::try_new() else {
            return;
        };

        // Test rapid repeated invocations of drain_transport_queue
        for _ in 0..50 {
            window.drain_transport_queue();
        }
        while context.iteration(false) {}

        // Test ReceiveAll action activation with flush-outbox true and false
        settings.set_flush_outbox(true);
        gio::prelude::ActionGroupExt::activate_action(&window, "mail-receive-all", None);
        while context.iteration(false) {}

        settings.set_flush_outbox(false);
        gio::prelude::ActionGroupExt::activate_action(&window, "mail-receive-all", None);
        while context.iteration(false) {}

        // Test exit purge repeatedly
        for _ in 0..10 {
            window.execute_exit_purge_and_expunge();
        }

        // Reset
        settings.set_flush_outbox(true);
        while context.iteration(false) {}
    }

    #[test]
    fn header_bar_has_search_button_and_no_transport_button() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let search_btn = window.search_button();
        assert!(search_btn.is_some(), "search_button must be present on window");
        let search_btn = search_btn.unwrap();
        assert_eq!(search_btn.icon_name().as_deref(), Some("edit-find-symbolic"));
        assert!(!search_btn.is_active());

        // Toggle search
        search_btn.set_active(true);
        assert!(window.is_active_search_open());

        search_btn.set_active(false);
        assert!(!window.is_active_search_open());

        // Switch to contacts view and test search toggle
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("contacts");
        }
        assert!(!window.is_active_search_open());
        search_btn.set_active(true);
        assert!(window.is_active_search_open());
        if let Some(contacts) = window.contacts_page() {
            assert!(contacts.is_search_active());
        }
        search_btn.set_active(false);
        assert!(!window.is_active_search_open());
        if let Some(contacts) = window.contacts_page() {
            assert!(!contacts.is_search_active());
        }

        // Ensure search_button is packed first (furthest to the left)
        let header_bar = window.imp().header_bar.borrow().as_ref().unwrap().clone();
        // Ensure search_button is in header_bar and is the first toggle button
        fn find_first_toggle_button(w: &gtk::Widget) -> Option<gtk::ToggleButton> {
            if let Some(btn) = w.downcast_ref::<gtk::ToggleButton>() {
                return Some(btn.clone());
            }
            let mut ch = w.first_child();
            while let Some(c) = ch {
                if let Some(found) = find_first_toggle_button(&c) {
                    return Some(found);
                }
                ch = c.next_sibling();
            }
            None
        }

        assert_eq!(
            find_first_toggle_button(header_bar.upcast_ref()),
            Some(search_btn.clone()),
            "search_button must be the first toggle button in header_bar"
        );

        // Ensure transport button (mail-send-receive-symbolic) and sidebar_button are not in headerbar
        let mut has_transport = false;
        let mut has_sidebar = false;
        fn check_widgets(w: &gtk::Widget, has_transport: &mut bool, has_sidebar: &mut bool) {
            if let Some(menu_btn) = w.downcast_ref::<gtk::MenuButton>() {
                if menu_btn.icon_name().as_deref() == Some("mail-send-receive-symbolic") {
                    *has_transport = true;
                }
            }
            if let Some(toggle_btn) = w.downcast_ref::<gtk::ToggleButton>() {
                if toggle_btn.icon_name().as_deref() == Some("sidebar-show-symbolic") {
                    *has_sidebar = true;
                }
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                check_widgets(&c, has_transport, has_sidebar);
                child = c.next_sibling();
            }
        }
        check_widgets(header_bar.upcast_ref(), &mut has_transport, &mut has_sidebar);
        assert!(!has_transport, "transport_button must not be in header_bar");
        assert!(!has_sidebar, "sidebar_button must not be in header_bar");
    }

    #[test]
    fn test_window_reload_accounts_and_find_meridian_window() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let found = find_meridian_window(&window);
        assert!(found.is_some());
        window.reload_accounts();
    }

    #[test]
    fn test_responsive_screen_modes_and_view_switcher_bar() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        let switcher_bar = window.view_switcher_bar().expect("view_switcher_bar should exist");
        let back_btn = window.back_button().expect("back_button should exist");
        let top_switcher = window.imp().view_switcher.borrow().clone().expect("view_switcher should exist");

        // Desktop default mode: wide policy
        assert_eq!(top_switcher.policy(), adw::ViewSwitcherPolicy::Wide);
        assert!(!switcher_bar.reveals(), "switcher_bar should not reveal on desktop");
        assert!(!back_btn.is_visible(), "back_button should be hidden initially");

        // Switch to tablet mode: switcher_bar revealed, top switcher remains wide to preserve sleek 46px headerbar
        window.set_screen_mode(false, true);
        assert_eq!(top_switcher.policy(), adw::ViewSwitcherPolicy::Wide);
        assert!(switcher_bar.reveals(), "switcher_bar should reveal on tablet per GNOME HIG");
        assert!(!back_btn.is_visible(), "back_button should remain hidden on tablet without drilldown");

        // Switch to mobile mode: bottom bar revealed, top switcher remains wide
        window.set_screen_mode(true, false);
        assert_eq!(top_switcher.policy(), adw::ViewSwitcherPolicy::Wide);
        assert!(switcher_bar.reveals(), "switcher_bar should reveal on mobile");
        assert!(!back_btn.is_visible(), "back_button should start hidden on mobile root");

        // Revert to desktop mode: returns to wide policy
        window.set_screen_mode(false, false);
        assert_eq!(top_switcher.policy(), adw::ViewSwitcherPolicy::Wide);
        assert!(!switcher_bar.reveals(), "switcher_bar should hide again on desktop");
    }

    #[test]
    fn test_responsive_mail_drilldown_navigation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let back_btn = window.back_button().expect("back_button should exist");
        let restore_btn = window.imp().restore_sidebar_button.borrow().clone().expect("restore button");
        let mail_page = window.mail_page().expect("mail_page should exist");

        // Ensure active view is mail for this test
        if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("mail");
        }

        // Activate mobile mode
        window.set_screen_mode(true, false);

        // Initially at Messages level (primary main view)
        assert_eq!(mail_page.drill_down_level(), crate::views::MailDrillDownLevel::Messages);
        assert!(!mail_page.can_navigate_back());
        assert!(!back_btn.is_visible());
        assert!(restore_btn.is_visible());

        // Toggle to Folders/Accounts
        mail_page.set_drill_down_level(crate::views::MailDrillDownLevel::Folders);
        assert!(mail_page.can_navigate_back());
        assert!(back_btn.is_visible());
        assert!(!restore_btn.is_visible());

        // Press back: Folders -> Messages
        assert!(window.handle_navigate_back());
        assert_eq!(mail_page.drill_down_level(), crate::views::MailDrillDownLevel::Messages);
        assert!(!mail_page.can_navigate_back());
        assert!(!back_btn.is_visible());
        assert!(restore_btn.is_visible());

        // Drill down to Viewer
        mail_page.set_drill_down_level(crate::views::MailDrillDownLevel::Viewer);
        assert!(mail_page.can_navigate_back());
        assert!(back_btn.is_visible());
        assert!(!restore_btn.is_visible());

        // Press back: Viewer -> Messages
        assert!(window.handle_navigate_back());
        assert_eq!(mail_page.drill_down_level(), crate::views::MailDrillDownLevel::Messages);
        assert!(!mail_page.can_navigate_back());
        assert!(!back_btn.is_visible());
        assert!(restore_btn.is_visible());

        // Press back at root: does nothing
        assert!(!window.handle_navigate_back());
    }

    #[test]
    fn test_window_navigate_back_closes_search() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("mail");
        }
        let mail_page = window.mail_page().expect("mail_page should exist");

        // Open search
        mail_page.toggle_search();
        assert!(mail_page.is_search_active());
        assert!(window.is_active_search_open());

        // Trigger navigate back (e.g. Escape key)
        assert!(window.handle_navigate_back());
        assert!(!mail_page.is_search_active());
        assert!(!window.is_active_search_open());
    }

    #[test]
    fn test_responsive_tablet_mode_drilldown_navigation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let back_btn = window.back_button().expect("back_button should exist");
        let mail_page = window.mail_page().expect("mail_page should exist");

        if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("mail");
        }

        // Activate tablet mode (550px - 900px)
        window.set_screen_mode(false, true);

        // Initially in 2-column browse mode: Folders + Messages visible, Viewer stacked/hidden
        assert!(!mail_page.can_navigate_back());
        assert!(!back_btn.is_visible());
        assert!(mail_page.imp().inner_paned.start_child().map(|w| w.is_visible()).unwrap_or(false));
        assert!(!mail_page.imp().inner_paned.end_child().map(|w| w.is_visible()).unwrap_or(true));

        // When user opens a message in tablet mode, drill down to Viewer
        mail_page.set_drill_down_level(crate::views::MailDrillDownLevel::Viewer);
        assert!(mail_page.can_navigate_back());
        assert!(back_btn.is_visible());
        assert!(!mail_page.imp().inner_paned.start_child().map(|w| w.is_visible()).unwrap_or(true));
        assert!(mail_page.imp().inner_paned.end_child().map(|w| w.is_visible()).unwrap_or(false));

        // Press back: Viewer -> Messages (returns to 2-column Folders + Messages)
        assert!(window.handle_navigate_back());
        assert!(!mail_page.can_navigate_back());
        assert!(!back_btn.is_visible());
        assert_eq!(mail_page.drill_down_level(), crate::views::MailDrillDownLevel::Messages);
        assert!(mail_page.imp().inner_paned.start_child().map(|w| w.is_visible()).unwrap_or(false));
        assert!(!mail_page.imp().inner_paned.end_child().map(|w| w.is_visible()).unwrap_or(true));

        // Press back again: does nothing in tablet mode
        assert!(!window.handle_navigate_back());
    }

    #[test]
    fn test_folder_properties_dialog_is_modal_adw_dialog() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let snapshot = crate::dialogs::FolderPropertiesSnapshot {
            folder_name: "Inbox".to_string(),
            account_name: "Personal".to_string(),
            folder_path: "INBOX".to_string(),
            total_count: 42,
            unread_count: 5,
            is_remote: true,
        };
        let dialog = crate::dialogs::FolderPropertiesDialog::new(snapshot);
        dialog.present(Some(&window));
        assert!(dialog.root().is_some());
        assert_eq!(dialog.root().as_ref(), window.root().as_ref());
    }

    #[test]
    fn test_event_dialog_modal_presentation_and_split() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let dialog = crate::dialogs::EventDialog::new();
        // Initially not modal until presented modal
        assert!(!dialog.is_modal());

        // Test modal presentation
        dialog.present_modal(&window);
        assert!(dialog.is_modal());
        assert!(dialog.root().is_some());
        assert_eq!(dialog.root().as_ref(), window.root().as_ref());
        let btn_open = dialog.imp().btn_open_in_new_window.borrow().clone();
        assert!(btn_open.is_some());
        assert!(btn_open.unwrap().is_visible());

        // Test splitting to standalone window
        dialog.split_to_standalone_window();
        assert!(dialog.imp().close_confirmed.get());

        // Test standalone presentation
        let standalone = crate::dialogs::EventDialog::new();
        assert!(!standalone.is_modal());
        standalone.present_standalone();
        assert!(!standalone.is_modal());
        let btn_standalone = standalone.imp().btn_open_in_new_window.borrow().clone();
        assert!(btn_standalone.is_some());
        assert!(!btn_standalone.unwrap().is_visible());
    }

    #[test]
    fn test_compose_dialog_modal_presentation_and_split() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let dialog = crate::dialogs::ComposeDialog::new();
        // Initially not modal until presented modal
        assert!(!dialog.is_modal());

        // Test modal presentation
        dialog.present_modal(&window);
        assert!(dialog.is_modal());
        assert_eq!(dialog.root().as_ref(), window.root().as_ref());
        let btn_open = dialog.imp().btn_open_in_new_window.borrow().clone();
        assert!(btn_open.is_some());
        assert!(btn_open.unwrap().is_visible());

        // Test splitting to standalone window
        dialog.split_to_standalone_window();
        assert!(dialog.imp().close_confirmed.get());
    }

    #[test]
    fn test_certificate_manager_dialog_is_modal_adw_dialog() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);

        let mgr = crate::dialogs::CertificateManagerDialog::new(Some(&window));
        mgr.present();
        assert!(mgr.dialog().root().is_some());
        assert_eq!(mgr.dialog().root().as_ref(), window.root().as_ref());
    }

    #[test]
    fn test_responsive_subpages_drilldown_navigation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        // Test memos page drilldown and toolbar
        let memos = crate::views::MemosPage::new();
        memos.set_toolbar_visible(true);
        assert!(memos.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        memos.set_screen_mode(true, false);
        assert!(!memos.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        assert!(!memos.can_navigate_back());
        memos.imp().is_detail_shown.set(true);
        assert!(memos.can_navigate_back());
        assert!(memos.navigate_back());
        assert!(!memos.can_navigate_back());
        assert!(!memos.navigate_back());
        memos.set_toolbar_visible(false);

        // Test tasks page drilldown and toolbar
        let tasks = crate::views::TasksPage::new();
        tasks.set_toolbar_visible(true);
        assert!(tasks.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        tasks.set_screen_mode(true, false);
        assert!(!tasks.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        assert!(!tasks.can_navigate_back());
        tasks.imp().is_detail_shown.set(true);
        assert!(tasks.can_navigate_back());
        assert!(tasks.navigate_back());
        assert!(!tasks.can_navigate_back());
        assert!(!tasks.navigate_back());
        tasks.set_toolbar_visible(false);

        // Test contacts page drilldown and toolbar
        let contacts = crate::views::ContactsPage::new();
        contacts.set_toolbar_visible(true);
        assert!(contacts.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        contacts.set_screen_mode(true, false);
        assert!(!contacts.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        assert!(!contacts.can_navigate_back());
        contacts.imp().is_detail_shown.set(true);
        assert!(contacts.can_navigate_back());
        assert!(contacts.navigate_back());
        assert!(!contacts.can_navigate_back());
        assert!(!contacts.navigate_back());
        contacts.set_toolbar_visible(false);

        // Test calendar page responsive mode and toolbar
        let calendar = crate::views::CalendarPage::new();
        assert!(calendar.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        calendar.set_screen_mode(true, false);
        // Calendar date navigation toolbar remains visible on mobile
        assert!(calendar.imp().toolbar.borrow().as_ref().unwrap().is_visible());
        assert!(!calendar.can_navigate_back());
    }

    #[test]
    fn test_sidebar_header_and_toggler_state() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view("mail");
            settings.set_mail_show_sidebar(true);
        }
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("mail");
        }
        window.set_screen_mode(false, false);

        let sidebar_header = window.imp().sidebar_header.borrow().clone().expect("sidebar_header should exist");
        let sidebar_title = window.imp().sidebar_title.borrow().clone().expect("sidebar_title should exist");
        let restore_btn = window.imp().restore_sidebar_button.borrow().clone().expect("restore_sidebar_button should exist");
        let close_btn = window.imp().sidebar_close_button.borrow().clone().expect("sidebar_close_button should exist");

        // Initial title in sidebar header is "All Inboxes" or "Inbox"
        assert!(
            sidebar_title.title().as_str() == "All Inboxes"
                || sidebar_title.title().as_str() == crate::i18n::gettext("Inbox")
        );

        // Update sidebar title test
        let cal_title = crate::i18n::gettext("Calendar");
        window.update_sidebar_title(&cal_title);
        assert_eq!(sidebar_title.title().as_str(), cal_title.as_str());

        // When sidebar is visible (default desktop):
        // sidebar_header is visible, restore_btn is hidden
        assert!(sidebar_header.is_visible());
        assert!(!restore_btn.is_visible());
        assert!(close_btn.is_visible());

        // In mobile mode: sidebar header is hidden; restore_btn is visible as mobile folder toggle for mail
        window.set_screen_mode(true, false);
        assert!(!sidebar_header.is_visible());
        assert!(restore_btn.is_visible());

        // In tablet mode: sidebar header is kept visible above the left sidebar, and restore_btn is hidden (close_btn in sidebar_header used)
        window.set_screen_mode(false, true);
        assert!(sidebar_header.is_visible());
        assert!(!restore_btn.is_visible());
        assert!(window.has_css_class("tablet-screen"));

        // Back to desktop:
        window.set_screen_mode(false, false);
        assert!(sidebar_header.is_visible());
        assert!(!restore_btn.is_visible());
        assert!(!window.has_css_class("tablet-screen"));
    }

    #[test]
    fn test_right_sidebar_toggle_and_per_view_state() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view("mail");
            settings.set_mail_show_sidebar(true);
        }
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);
        window.set_screen_mode(false, false);
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("mail");
        }
        assert_eq!(window.active_view_name(), "mail");

        let right_toggle = window
            .imp()
            .right_sidebar_toggle
            .borrow()
            .clone()
            .expect("right_sidebar_toggle should exist");

        // Icon should be sidebar-show-right-symbolic
        assert_eq!(
            right_toggle.icon_name().as_deref(),
            Some("sidebar-show-right-symbolic")
        );
        // On desktop, right panel toggle is visible
        assert!(right_toggle.is_visible());

        let agenda_toggle = window
            .imp()
            .agenda_preview_toggle
            .borrow()
            .clone()
            .expect("agenda_preview_toggle should exist");

        assert_eq!(
            agenda_toggle.icon_name().as_deref(),
            Some("sidebar-show-right-symbolic")
        );
        // Initially in mail view, agenda preview toggle must be hidden
        assert!(!agenda_toggle.is_visible());

        // In mobile mode, right panel toggle is hidden
        window.set_screen_mode(true, false);
        assert!(!right_toggle.is_visible());

        // Return to desktop
        window.set_screen_mode(false, false);
        assert!(right_toggle.is_visible());

        // Mail initial view: right panel is todo bar
        let initial_active = window.is_active_view_right_panel_visible();
        assert_eq!(right_toggle.is_active(), initial_active);

        // Toggle right panel
        window.toggle_active_view_right_panel();
        assert_eq!(
            window.is_active_view_right_panel_visible(),
            !initial_active
        );
        assert_eq!(right_toggle.is_active(), !initial_active);

        // Restore original state
        window.toggle_active_view_right_panel();
        assert_eq!(
            window.is_active_view_right_panel_visible(),
            initial_active
        );

        // Unified left sidebar test
        // 1. In mail, collapse sidebar
        window.toggle_sidebar_animated(false);
        assert!(!window.is_sidebar_visible_for_active_view());

        // 2. Switch to calendar: sidebar must remain collapsed (unified across views!)
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("calendar");
        }
        assert_eq!(window.active_view_name(), "calendar");
        assert!(!window.is_sidebar_visible_for_active_view());

        // 3. Switch to tasks: still collapsed
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("tasks");
        }
        assert_eq!(window.active_view_name(), "tasks");
        assert!(!window.is_sidebar_visible_for_active_view());

        // 4. Restore sidebar in tasks
        window.toggle_sidebar_animated(true);
        assert!(window.is_sidebar_visible_for_active_view());

        // 5. Switch back to mail: sidebar is restored in mail too
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("mail");
        }
        assert_eq!(window.active_view_name(), "mail");
        assert!(window.is_sidebar_visible_for_active_view());
    }

    #[test]
    fn test_sidebar_toggle_collapse_expand_alignment() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = gtk_test_main_context() else {
            return;
        };
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view("mail");
            settings.set_mail_show_sidebar(true);
            settings.set_mail_sidebar_position(260);
        }
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_visible(true);
        window.set_screen_mode(false, false);
        if let Some(stack) = window.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name("mail");
        }
        while context.iteration(false) {}

        let outer_paned = window.active_view_outer_paned().expect("outer paned");
        println!("Initial paned pos: {}", outer_paned.position());
        let sb_h = window.imp().sidebar_header.borrow().clone().expect("sb_h");
        println!("Initial sb_h size_request: {:?}", sb_h.size_request());

        window.toggle_sidebar_animated(false);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(!window.is_sidebar_visible_for_active_view());
        assert!(!sb_h.is_visible());
        if let Some(start) = outer_paned.start_child() {
            assert!(!start.is_visible());
        }
        if let Some(settings) = AppSettings::try_new() {
            assert_eq!(settings.mail_sidebar_position(), 260);
        }

        window.toggle_sidebar_animated(true);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(window.is_sidebar_visible_for_active_view());
        assert!(sb_h.is_visible());
        assert_eq!(outer_paned.position(), 260);
        if let Some(start) = outer_paned.start_child() {
            assert!(start.is_visible());
            assert_eq!(start.width(), 260);
        }
        assert_eq!(sb_h.size_request(), (260, -1));
        if let Some(settings) = AppSettings::try_new() {
            assert_eq!(settings.mail_sidebar_position(), 260);
        }
        // Cycle 2: Collapse and expand again
        window.toggle_sidebar_animated(false);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(!window.is_sidebar_visible_for_active_view());

        window.toggle_sidebar_animated(true);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(window.is_sidebar_visible_for_active_view());
        assert_eq!(outer_paned.position(), 260);
        assert_eq!(sb_h.size_request(), (260, -1));
        if let Some(start) = outer_paned.start_child() {
            assert_eq!(start.width(), 260);
        }

        // Cycle 3: If setting was corrupted to 125 while collapsed
        window.toggle_sidebar_animated(false);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_mail_sidebar_position(125);
        }
        window.toggle_sidebar_animated(true);
        for _ in 0..40 {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
            while context.iteration(false) {}
        }
        assert!(window.is_sidebar_visible_for_active_view());
        assert_eq!(outer_paned.position(), 240);
        assert_eq!(sb_h.size_request(), (240, -1));
        if let Some(start) = outer_paned.start_child() {
            assert_eq!(start.width(), 240);
        }
    }

    #[test]
    fn test_primary_action_button_label_and_responsiveness() {
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let _lock = GTK_TEST_MUTEX.lock().unwrap();

        let button = gtk::Button::new();
        // Desktop/Tablet: pure text label (GNOME HIG)
        MeridianWindow::configure_primary_action(&button, "mail", false);
        let child = button.child().expect("Button child");
        let label = child.downcast::<gtk::Label>().expect("Label child on desktop");
        assert_eq!(label.label().as_str(), crate::i18n::gettext("New Mail"));
        assert!(button.has_css_class("suggested-action"));

        // Switch to calendar: pure text label
        MeridianWindow::configure_primary_action(&button, "calendar", false);
        let child = button.child().expect("Button child");
        let label = child.downcast::<gtk::Label>().expect("Label child on desktop");
        assert_eq!(label.label().as_str(), crate::i18n::gettext("New Event"));

        // Switch to contacts: pure text label
        MeridianWindow::configure_primary_action(&button, "contacts", false);
        let child = button.child().expect("Button child");
        let label = child.downcast::<gtk::Label>().expect("Label child on desktop");
        assert_eq!(label.label().as_str(), crate::i18n::gettext("New Contact"));

        // Switch to tasks: pure text label
        MeridianWindow::configure_primary_action(&button, "tasks", false);
        let child = button.child().expect("Button child");
        let label = child.downcast::<gtk::Label>().expect("Label child on desktop");
        assert_eq!(label.label().as_str(), crate::i18n::gettext("New Task"));

        // Switch to memos: pure text label
        MeridianWindow::configure_primary_action(&button, "memos", false);
        let child = button.child().expect("Button child");
        let label = child.downcast::<gtk::Label>().expect("Label child on desktop");
        assert_eq!(label.label().as_str(), crate::i18n::gettext("New Memo"));

        // Compact / Mobile mode: pure icon '+' without text (GNOME HIG)
        MeridianWindow::configure_primary_action(&button, "mail", true);
        let child = button.child().expect("Button child");
        let img = child.downcast::<gtk::Image>().expect("Image child when compact");
        assert_eq!(img.icon_name().as_deref(), Some("list-add-symbolic"));
    }

    #[test]
    fn test_responsive_calendar_drilldown_and_navigation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let calendar = crate::views::CalendarPage::new();
        calendar.set_screen_mode(true, false);
        assert!(!calendar.can_navigate_back());

        // Select Day view from Month view in mobile mode -> should set drilldown
        calendar.imp().drill_down_previous_view.set(Some(crate::views::CalendarView::Month));
        assert!(calendar.can_navigate_back());

        // Navigate back should return to Month and clear drill down
        assert!(calendar.navigate_back());
        assert!(!calendar.can_navigate_back());
        assert!(!calendar.navigate_back());

        // Test period_label_text and current_view_title formatting
        let title = calendar.current_view_title();
        assert!(!title.is_empty());
        let period = calendar.period_label_text();
        assert!(!period.is_empty());
    }

    #[test]
    fn test_window_swipe_and_hardware_back_navigation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        window.set_screen_mode(true, false);

        // At default top-level, can_navigate_back is false
        assert!(!window.can_navigate_back());
        assert!(!window.handle_navigate_back());

        // Mail page drilldown
        if let Some(mail) = window.mail_page() {
            mail.imp().drill_down_level.set(crate::views::MailDrillDownLevel::Viewer);
            window.sync_back_button();
            assert!(window.can_navigate_back());

            // Handle back should transition Viewer -> Messages
            assert!(window.handle_navigate_back());
            assert_eq!(mail.drill_down_level(), crate::views::MailDrillDownLevel::Messages);

            // Handle back at Messages (root) returns false
            assert!(!window.handle_navigate_back());
            assert_eq!(mail.drill_down_level(), crate::views::MailDrillDownLevel::Messages);

            // Toggling sidebar button opens Folders
            mail.imp().drill_down_level.set(crate::views::MailDrillDownLevel::Folders);
            window.sync_back_button();
            assert!(window.can_navigate_back());
            assert!(window.handle_navigate_back());
            assert_eq!(mail.drill_down_level(), crate::views::MailDrillDownLevel::Messages);

            // Already at top level
            assert!(!window.can_navigate_back());
            assert!(!window.handle_navigate_back());
        }

        // Calendar drilldown back navigation
        if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("calendar");
            if let Some(cal) = window.calendar_page() {
                cal.imp().drill_down_previous_view.set(Some(crate::views::CalendarView::Month));
                window.sync_back_button();
                assert!(window.can_navigate_back());
                assert!(window.handle_navigate_back());
                assert!(!window.can_navigate_back());
            }

            // Contacts detail drilldown back navigation
            view_stack.set_visible_child_name("contacts");
            if let Some(contacts) = window.contacts_page() {
                contacts.imp().is_detail_shown.set(true);
                window.sync_back_button();
                assert!(window.can_navigate_back());
                assert!(window.handle_navigate_back());
                assert!(!contacts.is_detail_shown());
                assert!(!window.can_navigate_back());
            }

            // Tasks detail drilldown back navigation
            view_stack.set_visible_child_name("tasks");
            if let Some(tasks) = window.tasks_page() {
                tasks.imp().is_detail_shown.set(true);
                window.sync_back_button();
                assert!(window.can_navigate_back());
                assert!(window.handle_navigate_back());
                assert!(!tasks.is_detail_shown());
                assert!(!window.can_navigate_back());
            }

            // Memos detail drilldown back navigation
            view_stack.set_visible_child_name("memos");
            if let Some(memos) = window.memos_page() {
                memos.imp().is_detail_shown.set(true);
                window.sync_back_button();
                assert!(window.can_navigate_back());
                assert!(window.handle_navigate_back());
                assert!(!memos.is_detail_shown());
                assert!(!window.can_navigate_back());
            }
        }
    }

    #[test]
    fn test_mobile_headerbar_adaptation() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        // Desktop mode: no mobile-screen CSS class
        window.set_screen_mode(false, false);
        assert!(!window.has_css_class("mobile-screen"));

        // Mobile mode: adds mobile-screen CSS class
        window.set_screen_mode(true, false);
        assert!(window.has_css_class("mobile-screen"));

        // When can_navigate_back is false on mobile, compose_button visible property is true
        if let Some(compose_btn) = window.imp().compose_button.borrow().as_ref() {
            assert!(compose_btn.property::<bool>("visible"));
        }

        // When can_navigate_back is true on mobile, compose_button is hidden to maximize title room
        if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name("mail");
        }
        if let Some(mail) = window.mail_page() {
            mail.imp().drill_down_level.set(crate::views::MailDrillDownLevel::Viewer);
            window.sync_back_button();
            assert!(window.can_navigate_back());
            if let Some(compose_btn) = window.imp().compose_button.borrow().as_ref() {
                assert!(!compose_btn.property::<bool>("visible"));
            }

            // Window title should show email subject in viewer level
            if let Some(title) = window.imp().window_title.borrow().as_ref() {
                assert_eq!(title.subtitle().as_str(), crate::i18n::gettext("Message"));
            }
        }

        // Switching back to desktop mode removes mobile-screen class
        window.set_screen_mode(false, false);
        assert!(!window.has_css_class("mobile-screen"));
        if let Some(compose_btn) = window.imp().compose_button.borrow().as_ref() {
            assert!(compose_btn.property::<bool>("visible"));
        }
    }

    #[test]
    fn test_compact_and_mobile_screen_mode_transitions() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        // Verify initial state is desktop
        assert!(!window.is_mobile());
        assert!(!window.is_tablet());
        assert!(!window.is_compact());
        assert!(!window.has_css_class("mobile-screen"));
        assert!(!window.has_css_class("compact-screen"));

        // Transition to mobile
        window.set_screen_mode(true, false);
        assert!(window.is_mobile());
        assert!(!window.is_tablet());
        assert!(window.has_css_class("mobile-screen"));

        // Transition to compact mobile
        window.set_compact_screen(true);
        assert!(window.is_mobile());
        assert!(window.is_compact());
        assert!(window.has_css_class("compact-screen"));

        // Transition back to desktop should clear both classes
        window.set_screen_mode(false, false);
        assert!(!window.is_mobile());
        assert!(!window.is_compact());
        assert!(!window.has_css_class("mobile-screen"));
        assert!(!window.has_css_class("compact-screen"));

        // Libadwaita rule: If multiple breakpoints match, the last one added is used.
        // Broader breakpoints (tablet) must be registered before more specific ones (mobile, compact).
        let source = include_str!("window.rs");
        let idx_medium = source.find("self.add_breakpoint(bp_medium.clone());").expect("medium bp");
        let idx_tablet = source.find("self.add_breakpoint(bp_tablet.clone());").expect("tablet bp");
        let idx_mobile = source.find("self.add_breakpoint(bp_mobile.clone());").expect("mobile bp");
        let idx_compact = source.find("self.add_breakpoint(bp_compact.clone());").expect("compact bp");
        assert!(
            idx_medium < idx_tablet && idx_tablet < idx_mobile && idx_mobile < idx_compact,
            "Breakpoints must be registered in descending max-width order (broad to specific)"
        );
    }

    #[test]
    fn test_calendar_toolbar_remains_visible_and_adapts_on_mobile() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let calendar = crate::views::CalendarPage::new();

        // Initially in desktop mode
        calendar.set_screen_mode(false, false);
        let toolbar = calendar.imp().toolbar.borrow().as_ref().cloned().expect("toolbar");
        assert!(toolbar.is_visible(), "Toolbar must be visible in desktop mode");

        let dropdown = calendar.imp().calendar_view_dropdown.borrow().as_ref().cloned().expect("dropdown");
        assert!(!dropdown.is_visible(), "Dropdown should be hidden in desktop mode");

        let buttons = calendar.imp().calendar_view_buttons.borrow().as_ref().cloned().expect("buttons");
        let toggle_parent = buttons.first().and_then(|b| b.parent()).expect("view_toggle");
        assert!(toggle_parent.is_visible(), "Button bar should be visible in desktop mode");

        // Switch to mobile mode
        calendar.set_screen_mode(true, false);
        assert!(toolbar.is_visible(), "Toolbar MUST remain visible on mobile for date navigation");
        assert!(dropdown.is_visible(), "Dropdown must be visible on mobile");
        assert!(!toggle_parent.is_visible(), "Button bar must be hidden on mobile to prevent overflow");

        if let Some(date_label) = calendar.imp().month_date_label.borrow().as_ref() {
            assert!(!date_label.is_visible(), "Date label is hidden in toolbar on mobile as headerbar shows it");
        }

        // Verify GNOME HIG policy: No horizontal scrollbar on toolbar
        let scrolled = toolbar.clone().downcast::<gtk::ScrolledWindow>().expect("ScrolledWindow");
        assert_eq!(
            scrolled.hscrollbar_policy(),
            gtk::PolicyType::Never,
            "GNOME HIG requires Never for toolbar horizontal scrollbar"
        );

        // Switch to tablet mode
        calendar.set_screen_mode(false, true);
        assert!(dropdown.is_visible(), "Dropdown must be visible on tablet to prevent overflow");
        assert!(!toggle_parent.is_visible(), "Button bar must be hidden on tablet to prevent overflow");

        // Switch to medium mode
        calendar.set_screen_mode(false, false);
        calendar.set_medium_mode(true);
        assert!(dropdown.is_visible(), "Dropdown must be visible in medium mode to prevent overflow");
        assert!(!toggle_parent.is_visible(), "Button bar must be hidden in medium mode to prevent overflow");

        // Switch back to desktop mode
        calendar.set_medium_mode(false);
        assert!(!dropdown.is_visible(), "Dropdown should be hidden in full desktop mode");
        assert!(toggle_parent.is_visible(), "Button bar should be visible in full desktop mode");

        // Test compact mode adaptation on Today button
        calendar.apply_toolbar_button_modes(true);
        if let Some(today_btn) = calendar.imp().calendar_today_button.borrow().as_ref() {
            let child = today_btn.child().expect("child");
            assert!(child.downcast::<gtk::Image>().is_ok(), "Today button should use icon in compact mode");
        }

        calendar.apply_toolbar_button_modes(false);
        if let Some(today_btn) = calendar.imp().calendar_today_button.borrow().as_ref() {
            let child = today_btn.child().expect("child");
            assert!(child.downcast::<gtk::Label>().is_ok(), "Today button should use text in non-compact mode");
        }
    }

    #[test]
    fn test_mail_view_options_in_header_bar_and_memos_omitted_from_switcher() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let original_active_view = window.active_view_name();

        // 1. Mail view options button is present in window header_bar
        let view_btn = window
            .mail_view_options_button()
            .expect("mail_view_options_button should be registered on window");

        // Explicitly switch to mail to verify visibility
        window.set_active_view("mail");
        assert!(view_btn.property::<bool>("visible"), "View options button must be visible in mail view");

        // 2. Switching to calendar hides the mail view options button
        window.set_active_view("calendar");
        assert!(!view_btn.property::<bool>("visible"), "View options button must be hidden when switching away from mail");

        // 3. Switching back to mail reveals the button again
        window.set_active_view("mail");
        assert!(view_btn.property::<bool>("visible"), "View options button must be restored when returning to mail");

        // Restore original active view
        window.set_active_view(&original_active_view);

        // 4. ViewSwitcher pages: memos is not a titled tab, leaving exactly 4 core tabs
        let stack = window.imp().view_stack.borrow().clone().expect("view_stack");
        let pages = stack.pages();
        let mut titled_pages_count = 0;
        let mut page_titles = Vec::new();
        for i in 0..pages.n_items() {
            if let Some(p) = pages.item(i).and_downcast::<adw::ViewStackPage>() {
                if let Some(title) = p.title() {
                    if !title.is_empty() {
                        titled_pages_count += 1;
                        page_titles.push(title.to_string());
                    }
                }
            }
        }
        assert_eq!(titled_pages_count, 4, "ViewSwitcher must only have 4 tabs, but got: {:?}", page_titles);
        assert!(!page_titles.iter().any(|t| t.to_lowercase().contains("memo")), "Memos must be omitted from ViewSwitcher");
    }

    #[test]
    fn test_contacts_view_options_in_header_bar() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);
        let original_active_view = window.active_view_name();

        let mail_btn = window
            .mail_view_options_button()
            .expect("mail_view_options_button should be registered");

        // 1. Switching to contacts initializes contacts and attaches the contacts view options button
        window.set_active_view("contacts");
        let contacts_btn = window
            .contacts_view_options_button()
            .expect("contacts_view_options_button should be registered once contacts module loads");

        // In contacts view: contacts_btn is visible, mail_btn is hidden
        assert!(contacts_btn.property::<bool>("visible"), "Contacts view options button must be visible in contacts view");
        assert!(!mail_btn.property::<bool>("visible"), "Mail view options button must be hidden in contacts view");

        // 2. Switching to mail: mail_btn is visible, contacts_btn is hidden
        window.set_active_view("mail");
        assert!(mail_btn.property::<bool>("visible"), "Mail view options button must be visible in mail view");
        assert!(!contacts_btn.property::<bool>("visible"), "Contacts view options button must be hidden in mail view");

        // 3. Switching to calendar: both are hidden
        window.set_active_view("calendar");
        assert!(!mail_btn.property::<bool>("visible"), "Mail view options button must be hidden in calendar view");
        assert!(!contacts_btn.property::<bool>("visible"), "Contacts view options button must be hidden in calendar view");

        // 4. Switching back to contacts: contacts_btn is visible again
        window.set_active_view("contacts");
        assert!(contacts_btn.property::<bool>("visible"), "Contacts view options button must be restored in contacts view");
        assert!(!mail_btn.property::<bool>("visible"), "Mail view options button must be hidden in contacts view");

        window.set_active_view(&original_active_view);
    }

    #[test]
    fn test_show_help_overlay_action_presents_shortcuts_window() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        assert!(window.has_action("show-help-overlay"));
        let action = window
            .lookup_action("show-help-overlay")
            .expect("show-help-overlay action must be present on MeridianWindow");
        assert!(action.property::<bool>("enabled"));

        // Activating action creates and presents the shortcuts window for active view
        action.activate(None);
    }

    #[test]
    fn test_window_title_and_badge_unread_sync() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };
        let app = MeridianApplication::new();
        let window = MeridianWindow::new(&app);

        // Mail stack page must have been registered
        let mail_page = window.imp().mail_stack_page.borrow().as_ref().cloned();
        assert!(mail_page.is_some(), "mail_stack_page must be recorded on layout setup");

        // Verify sync_unread_counter_and_title runs without panic
        window.sync_unread_counter_and_title();

        // Check badge behavior: when no unread messages, badge_number is 0 and needs_attention is false
        if let Some(ref page) = mail_page {
            assert_eq!(page.badge_number(), 0);
            assert!(!page.needs_attention());
        }

        // Verify custom trailing pill badges in ViewSwitcher remain hidden in favor of minimal attention dot
        let badges = window.imp().mail_switcher_badges.borrow().clone();
        assert!(!badges.is_empty(), "mail_switcher_badges must have at least one badge attached");
        for b in &badges {
            assert!(!b.is_visible(), "Badge must be hidden when unread count is 0");
            assert!(b.has_css_class("mail-tab-badge"), "Badge must have mail-tab-badge class");
        }

        window.update_mail_unread_count(5);
        if let Some(ref page) = mail_page {
            assert!(page.needs_attention(), "needs_attention must be true when count > 0");
            assert_eq!(page.badge_number(), 0);
        }
        for b in &badges {
            assert!(!b.is_visible(), "Trailing text badge should remain hidden in favor of minimal attention dot");
        }

        window.update_mail_unread_count(0);
        if let Some(ref page) = mail_page {
            assert!(!page.needs_attention(), "needs_attention must be false when count is 0");
            assert_eq!(page.badge_number(), 0);
        }
    }
}



