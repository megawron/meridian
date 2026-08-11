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

use crate::application::EvolutionApplication;
use crate::services::AppSettings;
use crate::views::{CalendarPage, ContactsPage, MailPage};

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

    impl WidgetImpl for EvolutionWindow {}
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

    pub(crate) fn refresh_mail_page(&self) {
        if let Some(mail_page) = self.mail_page() {
            mail_page.refresh_mail();
        }
    }

    pub(crate) fn show_search_domain(&self, domain: crate::services::search_engine::SearchDomain) {
        let page_name = match domain {
            crate::services::search_engine::SearchDomain::Mail => "mail",
            crate::services::search_engine::SearchDomain::Calendar => "calendar",
            crate::services::search_engine::SearchDomain::Contacts => "contacts",
            crate::services::search_engine::SearchDomain::All => return,
        };
        if let Some(view_stack) = self.imp().view_stack.borrow().as_ref() {
            view_stack.set_visible_child_name(page_name);
        }
        self.add_toast(&adw::Toast::new(match page_name {
            "mail" => "Mailbereich geöffnet",
            "calendar" => "Kalender-/Aufgaben-/Notizbereich geöffnet",
            _ => "Kontaktbereich geöffnet",
        }));
    }

    fn setup_layout(&self) {
        let view_stack = adw::ViewStack::new();

        let mail_page = MailPage::new();
        view_stack.add_titled_with_icon(&mail_page, Some("mail"), "Mail", "mail-unread-symbolic");

        let calendar_page = CalendarPage::new();
        view_stack.add_titled_with_icon(
            &calendar_page,
            Some("calendar"),
            "Kalender",
            "x-office-calendar-symbolic",
        );

        let contacts_page = ContactsPage::new();
        view_stack.add_titled_with_icon(
            &contacts_page,
            Some("contacts"),
            "Kontakte",
            "contact-new-symbolic",
        );

        let tasks_page = crate::views::TasksPage::new();
        view_stack.add_titled_with_icon(
            &tasks_page,
            Some("tasks"),
            "Aufgaben",
            "tasks-due-symbolic",
        );

        let memos_page = crate::views::MemosPage::new();
        view_stack.add_titled_with_icon(&memos_page, Some("memos"), "Notizen", "note-symbolic");

        if let Some(settings) = AppSettings::try_new() {
            let active_view = settings.active_view();
            if matches!(
                active_view.as_str(),
                "mail" | "calendar" | "contacts" | "tasks" | "memos"
            ) {
                view_stack.set_visible_child_name(&active_view);
            }
            view_stack.connect_visible_child_name_notify(move |stack| {
                if let Some(view_name) = stack.visible_child_name() {
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

        header_bar.pack_end(&compose_button);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&view_stack));

        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.set_child(Some(&toolbar_view));

        // Responsive Breakpoints
        let bp_narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        if let Some(preview) = mail_page.preview_widget() {
            bp_narrow.add_setter(&preview, "visible", Some(&false.to_value()));
        }

        let bp_mobile =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 560sp").unwrap());
        bp_mobile.add_setter(mail_page.outer_paned(), "position", Some(&0_i32.to_value()));
        if let Some(preview) = mail_page.preview_widget() {
            bp_mobile.add_setter(&preview, "visible", Some(&false.to_value()));
        }
        bp_mobile.add_setter(&sidebar_button, "active", Some(&false.to_value()));

        self.add_breakpoint(bp_narrow);
        self.add_breakpoint(bp_mobile);

        self.imp().view_stack.replace(Some(view_stack));
        self.set_content(Some(toast_overlay));
    }

    fn build_primary_menu() -> gio::Menu {
        let menu = gio::Menu::new();

        let section1 = gio::Menu::new();
        section1.append(Some("Neue E-Mail"), Some("app.compose"));
        section1.append(Some("Lokale Entwürfe"), Some("app.drafts"));
        section1.append(Some("Globale Suche"), Some("app.global-search"));
        section1.append(Some("Ausgewählte E-Mail exportieren"), Some("win.export"));
        section1.append(Some("Mail aktualisieren"), Some("win.refresh-mail"));
        section1.append(Some("Rückgängig"), Some("win.undo"));
        section1.append(Some("Wiederholen"), Some("win.redo"));
        menu.append_section(None, &section1);

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
        menu.append_section(Some("Nachricht"), &message_menu);

        let section2 = gio::Menu::new();
        section2.append(Some("Einstellungen"), Some("app.preferences"));
        section2.append(Some("Tastenkombinationen"), Some("win.show-help-overlay"));
        section2.append(Some("Über Evolution"), Some("app.about"));
        menu.append_section(None, &section2);

        let view_menu = gio::Menu::new();
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
            Some("Datum in Nachrichtenzeilen ein-/ausblenden"),
            Some("win.toggle-mail-date"),
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

        menu
    }

    fn setup_actions(&self) {
        let action_sidebar = gio::SimpleAction::new("toggle-sidebar", None);
        action_sidebar.connect_activate(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _| {
                if let Some(view_stack) = window.imp().view_stack.borrow().as_ref() {
                    if let Some(mail_page) = view_stack
                        .child_by_name("mail")
                        .and_then(|w| w.downcast::<MailPage>().ok())
                    {
                        let current = mail_page.outer_paned().position();
                        let visible = current < 10;
                        mail_page.set_sidebar_visible(visible);
                        if let Some(settings) = AppSettings::try_new() {
                            settings.set_mail_show_sidebar(visible);
                        }
                    }
                }
            }
        ));
        self.add_action(&action_sidebar);

        let action_toggle_preview = gio::SimpleAction::new("toggle-preview", None);
        action_toggle_preview.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_show_message_preview(!settings.show_message_preview());
            }
        });
        self.add_action(&action_toggle_preview);

        let action_toggle_threading = gio::SimpleAction::new("toggle-mail-threading", None);
        action_toggle_threading.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_threading(!settings.mail_threading());
            }
        });
        self.add_action(&action_toggle_threading);

        let action_toggle_preview_text = gio::SimpleAction::new("toggle-mail-preview-text", None);
        action_toggle_preview_text.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_preview_text(!settings.mail_show_preview_text());
            }
        });
        self.add_action(&action_toggle_preview_text);

        let action_toggle_avatar = gio::SimpleAction::new("toggle-mail-avatar", None);
        action_toggle_avatar.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_avatar(!settings.mail_show_avatar());
            }
        });
        self.add_action(&action_toggle_avatar);

        let action_toggle_sender = gio::SimpleAction::new("toggle-mail-sender", None);
        action_toggle_sender.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_sender(!settings.mail_show_sender());
            }
        });
        self.add_action(&action_toggle_sender);

        let action_toggle_date = gio::SimpleAction::new("toggle-mail-date", None);
        action_toggle_date.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_date(!settings.mail_show_date());
            }
        });
        self.add_action(&action_toggle_date);

        let action_toggle_labels = gio::SimpleAction::new("toggle-mail-labels", None);
        action_toggle_labels.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_labels(!settings.mail_show_labels());
            }
        });
        self.add_action(&action_toggle_labels);

        for (action_name, density) in [
            ("mail-density-compact", "compact"),
            ("mail-density-normal", "comfortable"),
            ("mail-density-spacious", "spacious"),
        ] {
            let action = gio::SimpleAction::new(action_name, None);
            let density = density.to_string();
            action.connect_activate(move |_, _| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_row_density(&density);
                }
            });
            self.add_action(&action);
        }

        for (action_name, layout) in [
            ("use-classic-mail-layout", "classic"),
            ("use-modern-mail-layout", "modern"),
        ] {
            let action = gio::SimpleAction::new(action_name, None);
            let layout = layout.to_string();
            action.connect_activate(move |_, _| {
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_mail_layout(&layout);
                }
            });
            self.add_action(&action);
        }

        let action_toggle_mail_toolbar = gio::SimpleAction::new("toggle-mail-toolbar", None);
        action_toggle_mail_toolbar.connect_activate(move |_, _| {
            if let Some(settings) = AppSettings::try_new() {
                settings.set_mail_show_toolbar(!settings.mail_show_toolbar());
            }
        });
        self.add_action(&action_toggle_mail_toolbar);

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

        let action_search = gio::SimpleAction::new("search", None);
        action_search.connect_activate(glib::clone!(
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
                    mail_page.focus_search();
                }
            }
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
    }
}
