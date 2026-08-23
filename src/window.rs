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
use std::cell::Cell;
use std::rc::Rc;

use crate::application::EvolutionApplication;
use crate::dialogs::{ReminderDialog, ReminderDialogAction};
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
use crate::views::{CalendarPage, ContactsPage, MailPage, MemosPage, TasksPage};
use std::time::Instant;

// ─── GObject Subclass ───────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct EvolutionWindow {
        pub view_stack: RefCell<Option<adw::ViewStack>>,
        pub toast_overlay: adw::ToastOverlay,
        pub status_label: RefCell<Option<gtk::Label>>,
        pub status_progress: RefCell<Option<gtk::ProgressBar>>,
        pub online_toggle: RefCell<Option<gtk::Button>>,
        pub(crate) transport_service: MailTransportService,
        pub transport_generation: Cell<u64>,
        pub transport_queued_items: Cell<usize>,
        pub transport_status_source: RefCell<Option<glib::SourceId>>,
        pub reminder_dialog: RefCell<Option<ReminderDialog>>,
        pub component_open_generation: Cell<u64>,
        pub component_open_in_progress: Cell<bool>,
        pub component_open_target: RefCell<Option<ReminderIdentity>>,
        pub last_component_open: RefCell<Option<(ReminderIdentity, Instant)>>,
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
            window.imp().transport_service.stop();
            if let Some(source) = self.transport_status_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(mail_page) = window.mail_page() {
                mail_page.shutdown_print();
            }
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

    pub fn set_active_view(&self, view_name: &str) {
        if let Some(stack) = self.imp().view_stack.borrow().as_ref() {
            stack.set_visible_child_name(view_name);
        }
        if let Some(settings) = AppSettings::try_new() {
            settings.set_active_view(view_name);
        }
    }

    pub(crate) fn present_reminders(&self, deliveries: Vec<ReminderDelivery>) {
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
                        application.downcast::<EvolutionApplication>().ok()
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
        dialog.present_deliveries(&deliveries);
    }

    pub(crate) fn present_existing_reminders(&self) {
        if let Some(dialog) = self.imp().reminder_dialog.borrow().as_ref() {
            dialog.present_existing();
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
                    Some("Öffnen durch eine neuere Reminder-Aktion ersetzt".to_string()),
                );
            }
        }

        let page_name = match identity.kind {
            ReminderComponentKind::Event => "calendar",
            ReminderComponentKind::Task => "tasks",
            ReminderComponentKind::Memo => "memos",
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
                            Some(format!(
                                "Reminder-Quelle konnte nicht geprüft werden: {error}"
                            )),
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
                    "Reminder-Quelle ist nicht verfügbar oder wurde entfernt"
                }
                ReminderComponentPresence::MissingComponent => {
                    "Reminder-Komponente ist nicht verfügbar oder wurde geändert"
                }
                ReminderComponentPresence::Found => {
                    "Komponente konnte nach dem Laden nicht geöffnet werden"
                }
            };
            window.finish_reminder_open(identity, false, Some(message.to_string()));
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

    pub(crate) fn update_mail_status(&self, status_text: &str) {
        if let Some(label) = self.imp().status_label.borrow().as_ref() {
            let status_text = status_text.trim();
            if status_text.is_empty() {
                let offline = AppSettings::try_new()
                    .map(|s| s.offline_mode())
                    .unwrap_or(false);
                label.set_label(if offline {
                    "Evolution GTK4  ·  Offline"
                } else {
                    "Evolution GTK4  ·  Bereit"
                });
            } else {
                label.set_label(&format!("Evolution GTK4  ·  {status_text}"));
            }
        }
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
            toggle.set_tooltip_text(Some(if offline {
                "Offline-Modus aktiv; auf Online umschalten"
            } else {
                "Online-Modus aktiv; auf Offline umschalten"
            }));
        }

        if let Some(label) = self.imp().status_label.borrow().as_ref() {
            label.set_label(&format!("Evolution GTK4  ·  {}", snapshot.status_text()));
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
                            "Ein Mail-Transport läuft bereits"
                        }
                        MailTransportStartError::Offline => {
                            "Offline-Modus: Nachricht bleibt in der lokalen Warteschlange"
                        }
                        MailTransportStartError::NoQueuedMessages => {
                            "Keine ausstehenden Nachrichten"
                        }
                    };
                    self.add_toast(&adw::Toast::new(message));
                }
                return;
            }
        };

        self.update_transport_presentation(&self.imp().transport_service.snapshot());
        self.watch_mail_transport(operation, action);
    }

    fn drain_transport_queue(&self) {
        let Some(operation) = self.imp().transport_service.start_queued_if_online() else {
            return;
        };
        self.update_transport_presentation(&self.imp().transport_service.snapshot());
        self.watch_mail_transport(operation, MailTransportAction::SendAll);
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

    pub(crate) fn show_search_result(
        &self,
        domain: crate::services::search_engine::SearchDomain,
        reference_id: String,
        query: String,
    ) {
        match domain {
            crate::services::search_engine::SearchDomain::Mail => {
                self.show_search_result_loaded("mail", reference_id, query);
            }
            crate::services::search_engine::SearchDomain::Contacts => {
                self.show_search_result_loaded("contacts", reference_id, query);
            }
            crate::services::search_engine::SearchDomain::Calendar => {
                let reference_for_worker = reference_id.clone();
                let local_calendar_directory =
                    AppSettings::try_new().map(|settings| settings.local_calendar_directory());
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let target = reference_for_worker.split_once(':');
                    let snapshot = crate::services::reminder_service::calendar_store_for_worker(
                        local_calendar_directory,
                    )
                    .load_snapshot();
                    let page_name = if target.is_some_and(|(source_uid, uid)| {
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
                    };
                    let _ = sender.send_blocking(page_name);
                });
                let window = self.downgrade();
                glib::spawn_future_local(async move {
                    let Ok(page_name) = receiver.recv().await else {
                        return;
                    };
                    if let Some(window) = window.upgrade() {
                        window.show_search_result_loaded(page_name, reference_id, query);
                    }
                });
            }
            crate::services::search_engine::SearchDomain::All => {}
        }
    }

    fn show_search_result_loaded(
        &self,
        page_name: &'static str,
        reference_id: String,
        query: String,
    ) {
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
        let icon_theme = gtk::IconTheme::for_display(&button.display());
        button.set_icon_name(if icon_theme.has_icon(icon_name) {
            icon_name
        } else {
            "document-new-symbolic"
        });
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
            "toggle-all-headers",
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
            "toggle-mail-statusbar",
            "refresh-mail",
            "undo",
            "redo",
            "reply",
            "reply-all",
            "forward",
            "export",
            "mail-print",
            "mail-print-preview",
            "view-message-source",
            "find-in-message",
            "delete",
            "mark-unread",
            "mark-read",
            "select-all-mail",
            "toggle-flag",
            "toggle-important",
            "edit-labels",
            "move-message",
            "copy-message",
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
            "mail-print",
            "mail-print-preview",
            "view-message-source",
            "find-in-message",
            "delete",
            "mark-unread",
            "mark-read",
            "toggle-flag",
            "toggle-important",
            "edit-labels",
            "move-message",
            "copy-message",
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

    pub(crate) fn update_mail_print_actions(&self) {
        let is_mail_view = self
            .imp()
            .view_stack
            .borrow()
            .as_ref()
            .and_then(|view_stack| view_stack.visible_child_name())
            .is_some_and(|view_name| view_name == "mail");
        let has_selection = self
            .mail_page()
            .is_some_and(|mail_page| mail_page.has_selected_message());
        self.set_message_actions_enabled(is_mail_view && has_selection);
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

        // Module construction must not depend on GSettings being installed.
        // A direct `cargo run` has historically shown the permanent placeholder
        // for contacts/tasks/memos because the persistence layer is optional in
        // development builds.  Keep persistence optional, but always replace
        // the placeholder when the user navigates to a module.
        let ensure_module_loaded_for_signal = Rc::clone(&ensure_module_loaded);
        let settings_for_view = AppSettings::try_new();
        view_stack.connect_visible_child_name_notify(move |stack| {
            if let Some(view_name) = stack.visible_child_name() {
                ensure_module_loaded_for_signal(view_name.as_str());
                if let Some(settings) = settings_for_view.as_ref() {
                    settings.set_active_view(view_name.as_str());
                }
            }
        });

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

        let transport_button = gtk::MenuButton::builder()
            .icon_name("mail-send-receive-symbolic")
            .menu_model(&Self::build_transport_menu())
            .tooltip_text("Senden und Empfangen (F12)")
            .build();
        header_bar.pack_start(&transport_button);

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Narrow)
            .build();

        let window_title = adw::WindowTitle::new("Evolution", "");

        let title_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        title_stack.add_named(&window_title, Some("title"));
        title_stack.add_named(&view_switcher, Some("switcher"));

        let update_title_widget = {
            let title_stack = title_stack.clone();
            move || {
                let is_bottom = AppSettings::try_new()
                    .map(|s| s.switcher_location() == "bottom-left")
                    .unwrap_or(true);
                if is_bottom {
                    title_stack.set_visible_child_name("title");
                } else {
                    title_stack.set_visible_child_name("switcher");
                }
            }
        };

        update_title_widget();

        if let Some(settings) = AppSettings::try_new() {
            let update_cb = update_title_widget.clone();
            settings.connect_changed(Some("switcher-location"), move |_, _| {
                update_cb();
            });
        }

        header_bar.set_title_widget(Some(&title_stack));

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
            .icon_name("network-idle-symbolic")
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

        statusbar_box.append(&online_toggle);
        statusbar_box.append(&status_label);
        statusbar_box.append(&status_progress);

        self.imp().status_label.replace(Some(status_label.clone()));
        self.imp()
            .status_progress
            .replace(Some(status_progress.clone()));
        self.imp()
            .online_toggle
            .replace(Some(online_toggle.clone()));
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
        file_menu.append(Some("Senden & Empfangen"), Some("win.mail-send-receive"));
        file_menu.append(Some("Offline arbeiten"), Some("win.work-offline"));
        file_menu.append(Some("Online arbeiten"), Some("win.work-online"));
        file_menu.append(Some("Ausgewählte E-Mail exportieren"), Some("win.export"));
        file_menu.append(Some("Ausgewählte E-Mail drucken…"), Some("win.mail-print"));
        file_menu.append(
            Some("Druckvorschau der ausgewählten E-Mail…"),
            Some("win.mail-print-preview"),
        );
        menu.append_submenu(Some("Datei"), &file_menu);

        // ─── 2. Bearbeiten (Edit) ──────────────────────────────────────────
        let edit_menu = gio::Menu::new();
        edit_menu.append(Some("Rückgängig"), Some("win.undo"));
        edit_menu.append(Some("Wiederholen"), Some("win.redo"));
        edit_menu.append(Some("E-Mail suchen"), Some("win.search"));
        edit_menu.append(
            Some("In angezeigter Nachricht suchen…"),
            Some("win.find-in-message"),
        );
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
            Some("Alle Nachrichtenkopfzeilen anzeigen"),
            Some("win.toggle-all-headers"),
        );
        view_menu.append(
            Some("Nachrichtenquelle anzeigen"),
            Some("win.view-message-source"),
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
            Some("Statusinformationen unten ein-/ausblenden"),
            Some("win.toggle-mail-statusbar"),
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
        message_menu.append(Some("Als EML speichern..."), Some("win.export"));
        message_menu.append(Some("Drucken…"), Some("win.mail-print"));
        message_menu.append(Some("Druckvorschau…"), Some("win.mail-print-preview"));
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
        message_menu.append(Some("Labels bearbeiten..."), Some("win.edit-labels"));
        message_menu.append(Some("In Ordner verschieben..."), Some("win.move-message"));
        message_menu.append(Some("In Ordner kopieren..."), Some("win.copy-message"));
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

    fn build_transport_menu() -> gio::Menu {
        let menu = gio::Menu::new();
        menu.append(Some("Senden & Empfangen"), Some("win.mail-send-receive"));
        menu.append(Some("Alle Konten empfangen"), Some("win.mail-receive-all"));
        menu.append(
            Some("Ausstehende Nachrichten senden"),
            Some("win.mail-send-all"),
        );
        menu.append(Some("Abbrechen"), Some("win.mail-stop"));
        menu
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
                move |_, _| window.start_mail_transport(transport_action)
            ));
            self.add_action(&action);
        }

        let action_stop = gio::SimpleAction::new(MAIL_STOP_ACTION_ID, None);
        action_stop.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                if !window.imp().transport_service.stop() {
                    window.add_toast(&adw::Toast::new("Kein abbrechbarer Mail-Transport läuft"));
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
        self.add_boolean_setting_action(
            "toggle-mail-statusbar",
            "mail-show-statusbar",
            AppSettings::mail_show_statusbar,
            AppSettings::set_mail_show_statusbar,
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
                    win.add_toast(&adw::Toast::new("Keine Nachricht ausgewählt"));
                }
            }
        ));
        self.add_action(&action_message_source);

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

        let action_edit_labels = gio::SimpleAction::new("edit-labels", None);
        action_edit_labels.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if let Some(mail_page) = win.mail_page() {
                    mail_page.present_label_editor();
                } else {
                    win.add_toast(&adw::Toast::new("Mailansicht ist nicht verfügbar"));
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
                        win.add_toast(&adw::Toast::new("Mailansicht ist nicht verfügbar"));
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

        for (name, kind) in [
            ("open-event", ReminderComponentKind::Event),
            ("open-task", ReminderComponentKind::Task),
            ("open-memo", ReminderComponentKind::Memo),
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
                        win.add_toast(&adw::Toast::new("Ungültiger Reminder-Link"));
                        return;
                    };
                    if identity.kind != kind {
                        win.add_toast(&adw::Toast::new(
                            "Reminder-Link passt nicht zur angeforderten Komponente",
                        ));
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
                "Lokale app-eigene Nachrichten werden aus der sichtbaren Papierkorbansicht entfernt. Synchronisierte Ordner werden erst über den Linux-Pfad endgültig geleert.",
                "Papierkorb geleert",
            ),
            (
                "expunge",
                "Papierkorb endgültig leeren?",
                "Der lokale app-eigene Papierkorb wird geleert. Für synchronisierte Ordner ist der Linux-Pfad erforderlich.",
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

        let action_find_in_message = gio::SimpleAction::new("find-in-message", None);
        action_find_in_message.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                let opened = window
                    .mail_page()
                    .is_some_and(|mail_page| mail_page.find_in_displayed_message());
                if !opened {
                    window.add_toast(&adw::Toast::new(
                        "Für die Suche muss eine Nachricht im Vorschaufenster angezeigt sein",
                    ));
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
