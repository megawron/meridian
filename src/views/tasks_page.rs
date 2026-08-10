// SPDX-License-Identifier: GPL-3.0-or-later
//
// TasksPage — Dedicated Task and Todo (VTODO) manager view.
//
// Features:
// - Category list sidebar ("Alle Aufgaben", "Heute fällig", "Wichtig", "Erledigt")
// - Dynamic Task list with checkable boxes, priority chips, and due dates
// - "+ Neue Aufgabe" task creation dialog connection

use chrono::Utc;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::models::task_item::TaskItemObject;
use crate::services::calendar_store_service::{TaskRecord, update_ical_property};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use adw::prelude::*;
use adw::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct TasksPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TasksPage {
        const NAME: &'static str = "EvolutionTasksPage";
        type Type = super::TasksPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for TasksPage {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(cancel) = self.ecal_monitor_cancel.borrow_mut().take() {
                use std::sync::atomic::Ordering;
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TasksPage {}
}

glib::wrapper! {
    pub struct TasksPage(ObjectSubclass<imp::TasksPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TasksPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::new();

        // ── Sidebar (Categories) ────────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let header = gtk::Label::builder()
            .label("Aufgabenlisten")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        sidebar_box.append(&header);

        let empty_sidebar = gtk::Label::builder()
            .label("Aufgabenlisten werden nach der ECal-Anbindung angezeigt")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        sidebar_box.append(&empty_sidebar);

        let cat_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let categories: [(&str, &str, &str); 0] = [];

        for (icon, label, badge) in categories {
            let row = adw::ActionRow::builder()
                .title(label)
                .subtitle(badge)
                .build();
            let img = gtk::Image::from_icon_name(icon);
            row.add_prefix(&img);
            cat_list.append(&row);
        }

        sidebar_box.append(&cat_list);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar_box)
            .build();

        let sidebar_page = adw::NavigationPage::builder()
            .title("Aufgaben")
            .child(&sidebar_scroll)
            .build();

        split.set_sidebar(Some(&sidebar_page));

        // ── Main Content (Task List) ────────────────────────────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .build();

        let toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Aufgaben durchsuchen…")
            .hexpand(true)
            .build();

        let btn_add_task = gtk::Button::builder()
            .label("Neue Aufgabe")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["suggested-action"])
            .build();

        toolbar.append(&search_entry);
        toolbar.append(&btn_add_task);

        content_box.append(&toolbar);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let task_store = gio::ListStore::new::<TaskItemObject>();

        let task_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(vec!["boxed-list"])
            .build();

        for i in 0..task_store.n_items() {
            if let Some(task) = task_store.item(i).and_downcast::<TaskItemObject>() {
                let row = adw::ActionRow::builder()
                    .title(&task.title())
                    .subtitle(&format!(
                        "Fällig: {} · Priorität: {}",
                        task.due_date(),
                        task.priority()
                    ))
                    .build();

                let check = gtk::CheckButton::builder()
                    .active(task.is_completed())
                    .valign(gtk::Align::Center)
                    .build();

                let task_clone = task.clone();
                check.connect_toggled(move |c| {
                    task_clone.set_completed(c.is_active());
                });

                row.add_prefix(&check);
                task_list.append(&row);
            }
        }

        let empty_tasks = gtk::Label::builder()
            .label("Keine Aufgaben geladen")
            .css_classes(vec!["dim-label"])
            .margin_top(24)
            .build();
        content_box.append(&empty_tasks);

        let page_weak = self.downgrade();
        let task_store_for_dialog = task_store.clone();
        let task_list_for_dialog = task_list.clone();
        let empty_tasks_for_dialog = empty_tasks.clone();
        btn_add_task.connect_clicked(move |_| {
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            page.open_task_dialog(
                task_store_for_dialog.clone(),
                task_list_for_dialog.clone(),
                empty_tasks_for_dialog.clone(),
                None,
            );
        });

        let task_store_for_load = task_store.clone();
        let task_list_for_load = task_list.clone();
        let empty_tasks_for_load = empty_tasks.clone();
        let page_weak_for_load = self.downgrade();
        let (task_sender, task_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = load_snapshot_prefer_eds();
            if let Err(error) = task_sender.send_blocking(result) {
                tracing::debug!(%error, "Task snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match task_receiver.recv().await {
                Ok(result) => {
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Task data loaded with a backend warning");
                    }
                    if result.snapshot.tasks.is_empty() {
                        empty_tasks_for_load.set_label(match result.origin {
                            CalendarDataOrigin::Eds => "Keine Aufgaben aus EDS geladen",
                            CalendarDataOrigin::Local => "Keine lokalen Aufgaben geladen",
                        });
                        empty_tasks_for_load.set_visible(true);
                    } else {
                        empty_tasks_for_load.set_visible(false);
                        let Some(page) = page_weak_for_load.upgrade() else {
                            return;
                        };
                        for task in result.snapshot.tasks {
                            upsert_task_row(
                                &page,
                                &task_list_for_load,
                                &task_store_for_load,
                                &empty_tasks_for_load,
                                task,
                            );
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "Task snapshot unavailable"),
            }
        });

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let task_store_for_monitor = task_store.clone();
            let task_list_for_monitor = task_list.clone();
            let empty_tasks_for_monitor = empty_tasks.clone();
            let page_weak_for_monitor = self.downgrade();
            glib::spawn_future_local(async move {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        ECalSyncEvent::Ready(status) => {
                            if let Some(warning) = status.warning {
                                tracing::warn!(
                                    configured = status.configured_sources,
                                    active = status.active_sources,
                                    failed = status.failed_sources,
                                    %warning,
                                    "ECal task signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => remove_task_record(
                                &task_store_for_monitor,
                                &task_list_for_monitor,
                                &empty_tasks_for_monitor,
                                &change.source_uid,
                                &change.uid,
                            ),
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                let Some(page) = page_weak_for_monitor.upgrade() else {
                                    break;
                                };
                                if let Some(ECalRecord::Task(task)) = parse_change_record(&change) {
                                    upsert_task_row(
                                        &page,
                                        &task_list_for_monitor,
                                        &task_store_for_monitor,
                                        &empty_tasks_for_monitor,
                                        task,
                                    );
                                }
                            }
                        },
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal task signal monitor stopped");
                            break;
                        }
                    }
                }
            });
        }

        let task_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&task_list)
            .vexpand(true)
            .build();

        content_box.append(&task_scroll);

        let content_page = adw::NavigationPage::builder()
            .title("Aufgaben-Übersicht")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(260.0);
        split.set_max_sidebar_width(360.0);
        split.set_parent(self);
    }

    fn open_task_dialog(
        &self,
        task_store: gio::ListStore,
        task_list: gtk::ListBox,
        empty_tasks: gtk::Label,
        task_to_edit: Option<TaskItemObject>,
    ) {
        let dialog_title = if task_to_edit.is_some() {
            "Aufgabe bearbeiten"
        } else {
            "Neue Aufgabe"
        };
        let dialog = adw::Window::builder()
            .title(dialog_title)
            .modal(true)
            .default_width(540)
            .default_height(520)
            .build();
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
        }

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let save_button = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header_bar.pack_end(&save_button);
        toolbar_view.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let source_model = gtk::StringList::new(&["EDS-Aufgabenlisten werden geladen…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Aufgabenliste")
            .model(&source_model)
            .build();
        source_row.set_sensitive(task_to_edit.is_none());
        let details = adw::PreferencesGroup::builder().title("Details").build();
        details.add(&source_row);

        let title_entry = adw::EntryRow::builder().title("Titel").build();
        details.add(&title_entry);
        let due_entry = adw::EntryRow::builder()
            .title("Fällig am (YYYY-MM-DD, optional)")
            .build();
        details.add(&due_entry);

        let priority_row = adw::ActionRow::builder()
            .title("Priorität (1 = hoch, 0 = normal)")
            .build();
        let priority_spin = gtk::SpinButton::with_range(0.0, 9.0, 1.0);
        priority_spin.set_numeric(true);
        priority_spin.set_value(0.0);
        priority_row.add_suffix(&priority_spin);
        details.add(&priority_row);
        content_box.append(&details);

        let description_group = adw::PreferencesGroup::builder()
            .title("Beschreibung")
            .build();
        let description_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(120)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        description_group.add(&description_view);
        content_box.append(&description_group);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();
        toolbar_view.set_content(Some(&scroll));
        dialog.set_content(Some(&toolbar_view));

        let edit_source_uid = task_to_edit
            .as_ref()
            .map(|task| task.source_uid())
            .filter(|uid| !uid.is_empty());
        if let Some(task) = task_to_edit.as_ref() {
            title_entry.set_text(&task.title());
            due_entry.set_text(&task.due_date());
            let priority = task.priority().parse::<f64>().unwrap_or(0.0);
            priority_spin.set_value(priority);
            description_view
                .buffer()
                .set_text(&ical_property(&task.component(), "DESCRIPTION").unwrap_or_default());
        }

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let edit_source_uid_for_load = edit_source_uid.clone();
        glib::spawn_future_local(async move {
            match eds_service::list_calendar_sources().await {
                Ok(sources) => {
                    let active_sources: Vec<_> = sources
                        .into_iter()
                        .filter(|source| source.enabled)
                        .collect();
                    let names: Vec<String> = active_sources
                        .iter()
                        .map(|source| {
                            if source.backend_name.is_empty() {
                                source.display_name.clone()
                            } else {
                                format!("{} · {}", source.display_name, source.backend_name)
                            }
                        })
                        .collect();
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
                    let mut uids = source_uids_for_load.borrow_mut();
                    uids.clear();
                    uids.extend(active_sources.into_iter().map(|source| source.uid));
                    if let Some(edit_source_uid) = edit_source_uid_for_load.as_deref() {
                        if let Some(index) = uids.iter().position(|uid| uid == edit_source_uid) {
                            source_row_for_load.set_selected(index as u32);
                        }
                    }
                    if uids.is_empty() {
                        source_model_for_load.splice(
                            0,
                            source_model_for_load.n_items(),
                            &["Keine aktive EDS-Aufgabenliste verfügbar"],
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS task lists for task dialog");
                    source_model_for_load.splice(
                        0,
                        source_model_for_load.n_items(),
                        &["EDS-Aufgabenlisten sind nicht erreichbar"],
                    );
                }
            }
        });

        let dialog_weak = dialog.downgrade();
        let page_weak = self.downgrade();
        let source_uids_for_save = Rc::clone(&source_uids);
        let task_to_edit_for_save = task_to_edit.clone();
        let save_button_for_signal = save_button.clone();
        let save_button_for_closure = save_button.clone();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let source_uid = if let Some(task) = task_to_edit_for_save.as_ref() {
                task.source_uid()
            } else {
                let selected = source_row.selected() as usize;
                let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() else {
                    show_toast_for_widget(&dialog, "Keine aktive EDS-Aufgabenliste ist ausgewählt");
                    return;
                };
                source_uid
            };
            if source_uid.is_empty() {
                show_toast_for_widget(
                    &dialog,
                    "Lokale Aufgaben können ohne aktive EDS-Aufgabenliste nicht geändert werden",
                );
                return;
            };
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let priority = priority_spin.value() as u8;
            let completed = task_to_edit_for_save
                .as_ref()
                .is_some_and(TaskItemObject::is_completed);
            let component_result = match task_to_edit_for_save.as_ref() {
                Some(task) => update_task_component(
                    &task.component(),
                    &title_entry.text(),
                    &due_entry.text(),
                    priority,
                    &description,
                    completed,
                ),
                None => build_task_component(
                    &title_entry.text(),
                    &due_entry.text(),
                    priority,
                    &description,
                ),
            };
            let component = match component_result {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&dialog, &error);
                    return;
                }
            };
            let component_for_model = component.clone();
            let title_for_model = title_entry.text().trim().to_string();
            let due_for_model = due_entry.text().trim().to_string();
            let priority_for_model = if priority == 0 {
                "normal".to_string()
            } else {
                priority.to_string()
            };
            let source_uid_for_model = source_uid.clone();
            let operation_uid = task_to_edit_for_save.as_ref().map(TaskItemObject::id);
            let task_to_edit_for_result = task_to_edit_for_save.clone();
            let (sender, receiver) = async_channel::bounded(1);
            save_button_for_closure.set_sensitive(false);
            std::thread::spawn(move || {
                let service = ECalService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_component(
                        &source_uid,
                        ECalComponentType::Task,
                        uid,
                        &component,
                    ),
                    None => {
                        service.create_component(&source_uid, ECalComponentType::Task, &component)
                    }
                };
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Task mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog_weak.clone();
            let page_weak_for_result = page_weak.clone();
            let task_store_for_result = task_store.clone();
            let task_list_for_result = task_list.clone();
            let empty_tasks_for_result = empty_tasks.clone();
            let save_button_for_result = save_button_for_closure.clone();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                match result {
                    Ok(result) => {
                        let uid = result.uid;
                        let component = result.component.unwrap_or(component_for_model.clone());
                        if let Some(existing) = task_to_edit_for_result.as_ref() {
                            let mut store_index = None;
                            for index in 0..task_store_for_result.n_items() {
                                if task_store_for_result
                                    .item(index)
                                    .and_downcast::<TaskItemObject>()
                                    .is_some_and(|task| {
                                        task.id() == existing.id()
                                            && task.source_uid() == existing.source_uid()
                                    })
                                {
                                    store_index = Some(index);
                                    break;
                                }
                            }
                            if let Some(index) = store_index {
                                task_store_for_result.remove(index);
                            }
                            remove_task_row(
                                &task_list_for_result,
                                &existing.source_uid(),
                                &existing.id(),
                            );
                        }
                        let task = TaskItemObject::new(
                            &source_uid_for_model,
                            &uid,
                            &title_for_model,
                            &due_for_model,
                            &priority_for_model,
                            completed,
                            &component,
                        );
                        task_store_for_result.append(&task);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            append_task_row(
                                &page,
                                &task_list_for_result,
                                &task_store_for_result,
                                &empty_tasks_for_result,
                                &task,
                            );
                            empty_tasks_for_result.set_visible(false);
                            let message = if task_to_edit_for_result.is_some() {
                                format!("Aufgabe aktualisiert ({uid})")
                            } else {
                                format!("Aufgabe gespeichert ({uid})")
                            };
                            page.show_toast(&message);
                        }
                        dialog.close();
                    }
                    Err(error) => {
                        save_button_for_result.set_sensitive(true);
                        show_toast_for_widget(
                            &dialog,
                            &format!("Aufgabe konnte nicht gespeichert werden: {error}"),
                        );
                    }
                }
                save_button_for_result.set_sensitive(true);
            });
        });

        dialog.present();
    }

    fn show_toast(&self, message: &str) {
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
                window.add_toast(&adw::Toast::new(message));
                return;
            }
        }
        tracing::warn!(%message, "Task page result");
    }
}

fn upsert_task_row(
    page: &TasksPage,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    empty_tasks: &gtk::Label,
    task: TaskRecord,
) {
    let existing_index = (0..task_store.n_items()).find(|index| {
        task_store
            .item(*index)
            .and_downcast::<TaskItemObject>()
            .is_some_and(|existing| {
                existing.id() == task.uid && existing.source_uid() == task.source_uid
            })
    });
    if let Some(index) = existing_index {
        task_store.remove(index);
        remove_task_row(task_list, &task.source_uid, &task.uid);
    }

    let priority = if task.priority == 0 {
        "normal".to_string()
    } else {
        task.priority.to_string()
    };
    let task_object = TaskItemObject::new(
        &task.source_uid,
        &task.uid,
        &task.summary,
        &task.due,
        &priority,
        task.completed,
        &task.component,
    );
    task_store.append(&task_object);
    append_task_row(page, task_list, task_store, empty_tasks, &task_object);
    empty_tasks.set_visible(false);
}

fn remove_task_record(
    task_store: &gio::ListStore,
    task_list: &gtk::ListBox,
    empty_tasks: &gtk::Label,
    source_uid: &str,
    task_uid: &str,
) {
    let store_index = (0..task_store.n_items()).find(|index| {
        task_store
            .item(*index)
            .and_downcast::<TaskItemObject>()
            .is_some_and(|task| task.id() == task_uid && task.source_uid() == source_uid)
    });
    if let Some(index) = store_index {
        task_store.remove(index);
        remove_task_row(task_list, source_uid, task_uid);
    }
    empty_tasks.set_visible(task_store.n_items() == 0);
}

fn append_task_row(
    page: &TasksPage,
    task_list: &gtk::ListBox,
    task_store: &gio::ListStore,
    empty_tasks: &gtk::Label,
    task: &TaskItemObject,
) {
    let row = adw::ActionRow::builder()
        .title(&task.title())
        .subtitle(&format!(
            "Fällig: {} · Priorität: {}",
            task.due_date(),
            task.priority()
        ))
        .build();
    unsafe {
        row.set_data("task-id", task.id());
        row.set_data("task-source-uid", task.source_uid());
    }

    let check = gtk::CheckButton::builder()
        .active(task.is_completed())
        .valign(gtk::Align::Center)
        .build();
    let task_clone = task.clone();
    let source_uid = task.source_uid();
    let updating = Rc::new(Cell::new(false));
    let updating_for_callback = Rc::clone(&updating);
    check.connect_toggled(move |button| {
        if updating_for_callback.get() {
            return;
        }
        let completed = button.is_active();
        if source_uid.is_empty() {
            updating_for_callback.set(true);
            button.set_active(!completed);
            updating_for_callback.set(false);
            show_toast_for_widget(
                button,
                "Lokale Aufgaben können ohne EDS-Quelle nicht geändert werden",
            );
            return;
        }

        let status = if completed {
            "COMPLETED"
        } else {
            "NEEDS-ACTION"
        };
        let percent = if completed { "100" } else { "0" };
        let mut component = update_ical_property(&task_clone.component(), "STATUS", Some(status));
        component = update_ical_property(&component, "PERCENT-COMPLETE", Some(percent));
        component = if completed {
            update_ical_property(
                &component,
                "COMPLETED",
                Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
            )
        } else {
            update_ical_property(&component, "COMPLETED", None)
        };
        let component_for_result = component.clone();
        let uid = task_clone.id();
        let source_uid_for_worker = source_uid.clone();
        button.set_sensitive(false);
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = ECalService::new().modify_component(
                &source_uid_for_worker,
                ECalComponentType::Task,
                &uid,
                &component,
            );
            if let Err(error) = sender.send_blocking(result) {
                tracing::debug!(%error, "Task status mutation result was dropped");
            }
        });

        let task_for_result = task_clone.clone();
        let button_for_result = button.clone();
        let updating_for_result = Rc::clone(&updating_for_callback);
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            match result {
                Ok(result) => {
                    task_for_result.set_completed(completed);
                    task_for_result.set_component(result.component.unwrap_or(component_for_result));
                }
                Err(error) => {
                    updating_for_result.set(true);
                    button_for_result.set_active(!completed);
                    updating_for_result.set(false);
                    show_toast_for_widget(
                        &button_for_result,
                        &format!("Aufgabe konnte nicht aktualisiert werden: {error}"),
                    );
                }
            }
            button_for_result.set_sensitive(true);
        });
    });
    row.add_prefix(&check);

    let edit_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Aufgabe bearbeiten")
        .css_classes(vec!["flat"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&edit_button);
    let page_weak_for_edit = page.downgrade();
    let task_for_edit = task.clone();
    let task_store_for_edit = task_store.clone();
    let task_list_for_edit = task_list.clone();
    let empty_tasks_for_edit = empty_tasks.clone();
    let edit_button_for_signal = edit_button.clone();
    let edit_button_for_closure = edit_button.clone();
    edit_button_for_signal.connect_clicked(move |_| {
        if task_for_edit.source_uid().is_empty() {
            show_toast_for_widget(
                &edit_button_for_closure,
                "Lokale Aufgaben können ohne EDS-Quelle nicht geändert werden",
            );
            return;
        }
        let Some(page) = page_weak_for_edit.upgrade() else {
            return;
        };
        page.open_task_dialog(
            task_store_for_edit.clone(),
            task_list_for_edit.clone(),
            empty_tasks_for_edit.clone(),
            Some(task_for_edit.clone()),
        );
    });

    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Aufgabe löschen")
        .css_classes(vec!["flat", "destructive-action"])
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&delete_button);
    let task_for_delete = task.clone();
    let task_store_for_delete = task_store.clone();
    let task_list_for_delete = task_list.clone();
    let empty_tasks_for_delete = empty_tasks.clone();
    let delete_button_for_signal = delete_button.clone();
    delete_button_for_signal.connect_clicked(move |_| {
        let source_uid = task_for_delete.source_uid();
        if source_uid.is_empty() {
            show_toast_for_widget(
                &delete_button,
                "Lokale Aufgaben können ohne EDS-Quelle nicht gelöscht werden",
            );
            return;
        }
        let confirmation = adw::AlertDialog::new(
            Some("Aufgabe löschen?"),
            Some("Die Aufgabe wird dauerhaft aus der EDS-Aufgabenliste entfernt."),
        );
        confirmation.add_responses(&[("cancel", "Abbrechen"), ("delete", "Löschen")]);
        confirmation.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let task_for_response = task_for_delete.clone();
        let task_store_for_response = task_store_for_delete.clone();
        let task_list_for_response = task_list_for_delete.clone();
        let empty_tasks_for_response = empty_tasks_for_delete.clone();
        let delete_button_for_response = delete_button.clone();
        confirmation.connect_response(None, move |dialog: &adw::AlertDialog, response: &str| {
            if response != "delete" {
                return;
            }
            let source_uid = task_for_response.source_uid();
            let uid = task_for_response.id();
            let (sender, receiver) = async_channel::bounded(1);
            delete_button_for_response.set_sensitive(false);
            std::thread::spawn(move || {
                let result =
                    ECalService::new().remove_component(&source_uid, ECalComponentType::Task, &uid);
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Task deletion result was dropped");
                }
            });

            let dialog_for_result = dialog.clone();
            let task_store_for_result = task_store_for_response.clone();
            let task_list_for_result = task_list_for_response.clone();
            let empty_tasks_for_result = empty_tasks_for_response.clone();
            let delete_button_for_result = delete_button_for_response.clone();
            glib::spawn_future_local(async move {
                match receiver.recv().await {
                    Ok(Ok(result)) => {
                        let uid = result.uid;
                        let mut store_index = None;
                        for index in 0..task_store_for_result.n_items() {
                            if task_store_for_result
                                .item(index)
                                .and_downcast::<TaskItemObject>()
                                .is_some_and(|task| task.id() == uid)
                            {
                                store_index = Some(index);
                                break;
                            }
                        }
                        if let Some(index) = store_index {
                            task_store_for_result.remove(index);
                        }
                        if let Some(row) = delete_button_for_result
                            .ancestor(adw::ActionRow::static_type())
                            .and_downcast::<adw::ActionRow>()
                        {
                            task_list_for_result.remove(&row);
                        }
                        empty_tasks_for_result.set_visible(task_store_for_result.n_items() == 0);
                        show_toast_for_widget(
                            &delete_button_for_result,
                            &format!("Aufgabe gelöscht ({uid})"),
                        );
                        dialog_for_result.close();
                    }
                    Ok(Err(error)) => {
                        delete_button_for_result.set_sensitive(true);
                        show_toast_for_widget(
                            &delete_button_for_result,
                            &format!("Aufgabe konnte nicht gelöscht werden: {error}"),
                        );
                    }
                    Err(error) => {
                        delete_button_for_result.set_sensitive(true);
                        tracing::debug!(%error, "Task deletion channel closed");
                    }
                }
            });
        });
        confirmation.present(Some(&delete_button));
    });
    task_list.append(&row);
}

fn remove_task_row(task_list: &gtk::ListBox, source_uid: &str, task_uid: &str) {
    let mut child = task_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("task-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_task_uid = row
                .data::<String>("task-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_task_uid == task_uid
        };
        if matches {
            task_list.remove(&row);
            return;
        }
    }
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Task UI result");
}

fn build_task_component(
    title: &str,
    due: &str,
    priority: u8,
    description: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Eine Aufgabe benötigt einen Titel".to_string());
    }
    let due = due.trim();
    if !due.is_empty() {
        chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d")
            .map_err(|_| "Das Fälligkeitsdatum muss YYYY-MM-DD entsprechen".to_string())?;
    }
    if priority > 9 {
        return Err("Die Priorität muss zwischen 0 und 9 liegen".to_string());
    }

    let mut component = vec![
        "BEGIN:VTODO".to_string(),
        format!("UID:evolution-gtk4-{}", uuid::Uuid::new_v4()),
        format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
        "STATUS:NEEDS-ACTION".to_string(),
        "PERCENT-COMPLETE:0".to_string(),
        format!("SUMMARY:{}", escape_ical_text(title)),
    ];
    if !due.is_empty() {
        component.push(format!("DUE;VALUE=DATE:{}", due.replace('-', "")));
    }
    if priority > 0 {
        component.push(format!("PRIORITY:{priority}"));
    }
    if !description.trim().is_empty() {
        component.push(format!(
            "DESCRIPTION:{}",
            escape_ical_text(description.trim())
        ));
    }
    component.push("END:VTODO".to_string());
    Ok(format!("{}\r\n", component.join("\r\n")))
}

fn update_task_component(
    existing: &str,
    title: &str,
    due: &str,
    priority: u8,
    description: &str,
    completed: bool,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Eine Aufgabe benötigt einen Titel".to_string());
    }
    let due = due.trim();
    if !due.is_empty() {
        chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d")
            .map_err(|_| "Das Fälligkeitsdatum muss YYYY-MM-DD entsprechen".to_string())?;
    }
    if priority > 9 {
        return Err("Die Priorität muss zwischen 0 und 9 liegen".to_string());
    }

    let mut component = update_ical_property(existing, "SUMMARY", Some(&escape_ical_text(title)));
    component = if due.is_empty() {
        update_ical_property(&component, "DUE", None)
    } else {
        update_ical_property(&component, "DUE;VALUE=DATE", Some(&due.replace('-', "")))
    };
    component = if priority == 0 {
        update_ical_property(&component, "PRIORITY", None)
    } else {
        update_ical_property(&component, "PRIORITY", Some(&priority.to_string()))
    };
    component = if description.trim().is_empty() {
        update_ical_property(&component, "DESCRIPTION", None)
    } else {
        update_ical_property(
            &component,
            "DESCRIPTION",
            Some(&escape_ical_text(description.trim())),
        )
    };
    component = update_ical_property(
        &component,
        "STATUS",
        Some(if completed {
            "COMPLETED"
        } else {
            "NEEDS-ACTION"
        }),
    );
    component = update_ical_property(
        &component,
        "PERCENT-COMPLETE",
        Some(if completed { "100" } else { "0" }),
    );
    component = if completed {
        update_ical_property(
            &component,
            "COMPLETED",
            Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
        )
    } else {
        update_ical_property(&component, "COMPLETED", None)
    };
    Ok(update_ical_property(
        &component,
        "DTSTAMP",
        Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
    ))
}

fn ical_property(raw: &str, property_name: &str) -> Option<String> {
    let target = property_name
        .split(';')
        .next()
        .unwrap_or(property_name)
        .to_ascii_uppercase();
    let mut lines: Vec<String> = Vec::new();
    for raw_line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(raw_line.trim_start());
            }
        } else {
            lines.push(raw_line.to_string());
        }
    }
    lines.into_iter().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        let property = property
            .split(';')
            .next()
            .unwrap_or(property)
            .to_ascii_uppercase();
        (property == target).then(|| unescape_ical_text(value))
    })
}

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_component_serialization_validates_and_escapes_fields() {
        let component =
            build_task_component("Prepare, report; Q4", "2026-08-11", 1, "Line one\nLine two")
                .expect("task component should be valid");
        assert!(component.starts_with("BEGIN:VTODO\r\n"));
        assert!(component.contains("SUMMARY:Prepare\\, report\\; Q4\r\n"));
        assert!(component.contains("DUE;VALUE=DATE:20260811\r\n"));
        assert!(component.contains("PRIORITY:1\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.ends_with("END:VTODO\r\n"));
    }

    #[test]
    fn task_component_rejects_invalid_due_date_and_empty_title() {
        assert!(build_task_component("", "2026-08-11", 0, "").is_err());
        assert!(build_task_component("Task", "11.08.2026", 0, "").is_err());
        assert!(build_task_component("Task", "", 10, "").is_err());
    }

    #[test]
    fn task_component_update_preserves_uid_and_nested_alarm_data() {
        let existing = concat!(
            "BEGIN:VTODO\r\n",
            "UID:task-1\r\n",
            "DTSTAMP:20260810T090000Z\r\n",
            "STATUS:NEEDS-ACTION\r\n",
            "SUMMARY:Old title\r\n",
            "DUE;VALUE=DATE:20260811\r\n",
            "DESCRIPTION:Old description\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "DESCRIPTION:Reminder\r\n",
            "END:VALARM\r\n",
            "END:VTODO\r\n"
        );
        let updated = update_task_component(
            existing,
            "New, title",
            "2026-08-20",
            2,
            "New description",
            false,
        )
        .expect("task update should be valid");
        assert!(updated.contains("UID:task-1\r\n"));
        assert!(updated.contains("SUMMARY:New\\, title\r\n"));
        assert!(updated.contains("DUE;VALUE=DATE:20260820\r\n"));
        assert!(updated.contains("PRIORITY:2\r\n"));
        assert!(updated.contains("DESCRIPTION:New description\r\n"));
        assert!(updated.contains("BEGIN:VALARM\r\n"));
        assert!(updated.contains("DESCRIPTION:Reminder\r\n"));
        assert!(updated.contains("STATUS:NEEDS-ACTION\r\n"));
        assert!(!updated.contains("COMPLETED:"));
    }

    #[test]
    fn task_description_parser_unescapes_folded_ical_values() {
        let raw = "BEGIN:VTODO\r\nDESCRIPTION:Line one\\nLine\r\n two\r\nEND:VTODO\r\n";
        assert_eq!(
            ical_property(raw, "DESCRIPTION").as_deref(),
            Some("Line one\nLinetwo")
        );
    }
}

impl Default for TasksPage {
    fn default() -> Self {
        Self::new()
    }
}
