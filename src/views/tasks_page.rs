// SPDX-License-Identifier: GPL-3.0-or-later
//
// TasksPage — Dedicated Task and Todo (VTODO) manager view.
//
// Features:
// - Category list sidebar ("Alle Aufgaben", "Heute fällig", "Wichtig", "Erledigt")
// - Dynamic Task list with checkable boxes, priority chips, and due dates
// - "+ Neue Aufgabe" task creation dialog connection

use crate::models::task_item::TaskItemObject;
use crate::services::calendar_store_service::CalendarStoreService;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct TasksPage;

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

        btn_add_task.connect_clicked(|_| {
            tracing::warn!("Add task requested, but the ECal backend is not connected");
        });

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

        let task_store_for_load = task_store.clone();
        let task_list_for_load = task_list.clone();
        let empty_tasks_for_load = empty_tasks.clone();
        let (task_sender, task_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let snapshot = CalendarStoreService::new().load_snapshot();
            if let Err(error) = task_sender.send_blocking(snapshot) {
                tracing::debug!(%error, "Task snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match task_receiver.recv().await {
                Ok(snapshot) => {
                    if snapshot.tasks.is_empty() {
                        empty_tasks_for_load.set_visible(true);
                    } else {
                        empty_tasks_for_load.set_visible(false);
                        for task in snapshot.tasks {
                            let priority = if task.priority == 0 {
                                "normal".to_string()
                            } else {
                                task.priority.to_string()
                            };
                            let task_object = TaskItemObject::new(
                                &task.uid,
                                &task.summary,
                                &task.due,
                                &priority,
                                task.completed,
                            );
                            task_store_for_load.append(&task_object);
                            append_task_row(&task_list_for_load, &task_object);
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "Task snapshot unavailable"),
            }
        });

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
}

fn append_task_row(task_list: &gtk::ListBox, task: &TaskItemObject) {
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
    check.connect_toggled(move |button| {
        task_clone.set_completed(button.is_active());
    });
    row.add_prefix(&check);
    task_list.append(&row);
}

impl Default for TasksPage {
    fn default() -> Self {
        Self::new()
    }
}
