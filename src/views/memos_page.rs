// SPDX-License-Identifier: GPL-3.0-or-later
//
// MemosPage — Dedicated Memos and Notes (VJOURNAL) manager view.
//
// Features:
// - Color-coded note entries
// - Memo list sidebar & detail text viewer
// - "+ Neue Notiz" memo creation button

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::Utc;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::update_ical_property;
use crate::services::calendar_store_service::{
    CalendarStoreService, MemoRecord, export_ical_component,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::settings_service::AppSettings;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MemosPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MemosPage {
        const NAME: &'static str = "EvolutionMemosPage";
        type Type = super::MemosPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for MemosPage {
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

    impl WidgetImpl for MemosPage {}
}

glib::wrapper! {
    pub struct MemosPage(ObjectSubclass<imp::MemosPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MemoSort {
    #[default]
    Updated,
    Title,
    Source,
}

impl MemoSort {
    fn from_setting(value: &str) -> Self {
        match value {
            "title" => Self::Title,
            "source" => Self::Source,
            _ => Self::Updated,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Title => "title",
            Self::Source => "source",
        }
    }
}

impl MemosPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::new();

        let memo_sort = Rc::new(Cell::new(
            AppSettings::try_new()
                .map(|settings| MemoSort::from_setting(&settings.memo_sort_field()))
                .unwrap_or_default(),
        ));
        let memo_sort_descending = Rc::new(Cell::new(
            AppSettings::try_new()
                .map(|settings| settings.memo_sort_descending())
                .unwrap_or(true),
        ));

        // ── Sidebar (Memo List) ─────────────────────────────────────────

        let sidebar_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Notizen durchsuchen…")
            .build();
        let sort_model = gtk::StringList::new(&["Zuletzt geändert", "Titel", "Quelle"]);
        let sort_selector = gtk::DropDown::new(Some(sort_model), None::<&gtk::Expression>);
        sort_selector.set_tooltip_text(Some("Notizen sortieren"));
        sort_selector.set_selected(match memo_sort.get() {
            MemoSort::Updated => 0,
            MemoSort::Title => 1,
            MemoSort::Source => 2,
        });

        let direction_button = gtk::Button::builder()
            .css_classes(vec!["flat", "circular"])
            .build();
        let set_direction_button = |button: &gtk::Button, descending: bool| {
            button.set_icon_name(if descending {
                "view-sort-descending-symbolic"
            } else {
                "view-sort-ascending-symbolic"
            });
            button.set_tooltip_text(Some(if descending {
                "Absteigend sortiert"
            } else {
                "Aufsteigend sortiert"
            }));
        };
        set_direction_button(&direction_button, memo_sort_descending.get());
        search_entry.set_hexpand(true);
        let search_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_bottom(4)
            .build();
        search_toolbar.append(&search_entry);
        search_toolbar.append(&sort_selector);
        search_toolbar.append(&direction_button);
        sidebar_box.append(&search_toolbar);

        let memo_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let memo_sort_for_list = Rc::clone(&memo_sort);
        let memo_sort_descending_for_list = Rc::clone(&memo_sort_descending);
        memo_list.set_sort_func(move |left, right| {
            compare_memo_rows(
                left,
                right,
                memo_sort_for_list.get(),
                memo_sort_descending_for_list.get(),
            )
            .into()
        });

        let memo_sort_for_selector = Rc::clone(&memo_sort);
        let memo_list_for_sort = memo_list.clone();
        sort_selector.connect_selected_notify(move |selector| {
            let sort = match selector.selected() {
                1 => MemoSort::Title,
                2 => MemoSort::Source,
                _ => MemoSort::Updated,
            };
            memo_sort_for_selector.set(sort);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_sort_field(sort.setting_value());
            }
            memo_list_for_sort.invalidate_sort();
        });

        let memo_sort_descending_for_button = Rc::clone(&memo_sort_descending);
        let memo_list_for_direction = memo_list.clone();
        let direction_button_for_signal = direction_button.clone();
        direction_button.connect_clicked(move |_| {
            let descending = !memo_sort_descending_for_button.get();
            memo_sort_descending_for_button.set(descending);
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_sort_descending(descending);
            }
            set_direction_button(&direction_button_for_signal, descending);
            memo_list_for_direction.invalidate_sort();
        });

        let memo_records = Rc::new(RefCell::new(Vec::<MemoRecord>::new()));
        let selected_memo = Rc::new(RefCell::new(None::<MemoRecord>));

        let empty_state = gtk::Label::builder()
            .label("Keine Notizen aus EDS geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        sidebar_box.append(&empty_state);

        sidebar_box.append(&memo_list);

        let memo_records_for_search = Rc::clone(&memo_records);
        let empty_state_for_search = empty_state.clone();
        search_entry.connect_search_changed(glib::clone!(
            #[weak]
            memo_list,
            move |entry| {
                let query = entry.text().trim().to_lowercase();
                if query.is_empty() {
                    memo_list.set_filter_func(|_| true);
                    empty_state_for_search.set_label("Keine Notizen aus EDS geladen");
                } else {
                    let query_for_filter = query.clone();
                    memo_list.set_filter_func(move |row| unsafe {
                        row.data::<String>("memo-search")
                            .is_some_and(|value| value.as_ref().contains(&query_for_filter))
                    });
                    empty_state_for_search.set_label("Keine passenden Notizen");
                }
                let has_records = if query.is_empty() {
                    !memo_records_for_search.borrow().is_empty()
                } else {
                    memo_records_for_search
                        .borrow()
                        .iter()
                        .any(|memo| memo_search_text(memo).contains(&query))
                };
                empty_state_for_search.set_visible(!has_records);
            }
        ));

        let new_memo_btn = gtk::Button::builder()
            .label("Neue Notiz")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["suggested-action"])
            .margin_top(8)
            .build();
        sidebar_box.append(&new_memo_btn);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar_box)
            .build();

        let sidebar_page = adw::NavigationPage::builder()
            .title("Notizen")
            .child(&sidebar_scroll)
            .build();

        split.set_sidebar(Some(&sidebar_page));

        // ── Main Content (Memo Detail View) ──────────────────────────────

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .spacing(16)
            .build();

        let memo_title = gtk::Entry::builder()
            .placeholder_text("Keine Notiz ausgewählt")
            .css_classes(vec!["title-1"])
            .build();

        let memo_meta = gtk::Label::builder()
            .label("Notizen werden nach der ECal-Anbindung geladen")
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Start)
            .build();

        let memo_text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .margin_top(8)
            .build();
        memo_text
            .buffer()
            .set_text("Noch keine Notizdaten verfügbar.");

        let memo_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        memo_actions.append(&memo_title);
        memo_title.set_hexpand(true);
        let save_memo_btn = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        let delete_memo_btn = gtk::Button::builder()
            .label("Löschen")
            .css_classes(vec!["destructive-action"])
            .build();
        let export_memo_btn = gtk::Button::builder()
            .label("Exportieren")
            .icon_name("document-save-symbolic")
            .build();
        memo_actions.append(&save_memo_btn);
        memo_actions.append(&export_memo_btn);
        memo_actions.append(&delete_memo_btn);
        content_box.append(&memo_actions);
        content_box.append(&memo_meta);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content_box.append(&memo_text);

        let page_weak = self.downgrade();
        let memo_list_for_dialog = memo_list.clone();
        let memo_records_for_dialog = Rc::clone(&memo_records);
        let empty_state_for_dialog = empty_state.clone();
        new_memo_btn.connect_clicked(move |_| {
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            page.open_memo_dialog(
                memo_list_for_dialog.clone(),
                Rc::clone(&memo_records_for_dialog),
                empty_state_for_dialog.clone(),
            );
        });

        let selected_memo_for_save = Rc::clone(&selected_memo);
        let memo_records_for_save = Rc::clone(&memo_records);
        let page_weak_for_save = self.downgrade();
        let memo_title_for_save = memo_title.clone();
        let memo_text_for_save = memo_text.clone();
        let memo_list_for_save = memo_list.clone();
        let save_memo_btn_for_signal = save_memo_btn.clone();
        save_memo_btn_for_signal.connect_clicked(move |_| {
            let Some(memo) = selected_memo_for_save.borrow().clone() else {
                show_toast_for_widget(&memo_title_for_save, "Keine Notiz ausgewählt");
                return;
            };
            if memo.source_uid.is_empty() {
                show_toast_for_widget(
                    &memo_title_for_save,
                    "Diese Notiz stammt aus einer schreibgeschützten importierten Quelle",
                );
                return;
            }
            let title = memo_title_for_save.text().trim().to_string();
            if title.is_empty() {
                show_toast_for_widget(&memo_title_for_save, "Eine Notiz benötigt einen Titel");
                return;
            }
            let description = memo_text_for_save
                .buffer()
                .text(
                    &memo_text_for_save.buffer().start_iter(),
                    &memo_text_for_save.buffer().end_iter(),
                    false,
                )
                .to_string();
            let mut component =
                update_ical_property(&memo.component, "SUMMARY", Some(&escape_ical_text(&title)));
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
                "LAST-MODIFIED",
                Some(&Utc::now().format("%Y%m%dT%H%M%SZ").to_string()),
            );
            let component_for_result = component.clone();
            let source_uid = memo.source_uid.clone();
            let uid = memo.uid.clone();
            let (sender, receiver) = async_channel::bounded(1);
            save_memo_btn.set_sensitive(false);
            std::thread::spawn(move || {
                let result = ECalService::new().modify_component(
                    &source_uid,
                    ECalComponentType::Memo,
                    &uid,
                    &component,
                );
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Memo update result was dropped");
                }
            });

            let save_button_for_result = save_memo_btn.clone();
            let memo_records_for_result = Rc::clone(&memo_records_for_save);
            let memo_list_for_result = memo_list_for_save.clone();
            let selected_memo_for_result = Rc::clone(&selected_memo_for_save);
            let page_weak_for_result = page_weak_for_save.clone();
            let memo_title_for_result = memo_title_for_save.clone();
            let memo_text_for_result = memo_text_for_save.clone();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    save_button_for_result.set_sensitive(true);
                    return;
                };
                match result {
                    Ok(result) => {
                        let confirmed_component = result.component.unwrap_or(component_for_result);
                        let updated = MemoRecord {
                            uid: memo.uid.clone(),
                            source_uid: memo.source_uid.clone(),
                            source_display_name: memo.source_display_name.clone(),
                            component: confirmed_component,
                            summary: title.clone(),
                            description: description.clone(),
                            updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                        };
                        upsert_memo_row(
                            &memo_list_for_result,
                            Rc::clone(&memo_records_for_result),
                            updated.clone(),
                        );
                        *selected_memo_for_result.borrow_mut() = Some(updated);
                        memo_title_for_result.set_text(&title);
                        memo_text_for_result.buffer().set_text(&description);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            page.show_toast(&format!("Notiz aktualisiert ({})", memo.uid));
                        }
                    }
                    Err(error) => show_toast_for_widget(
                        &save_button_for_result,
                        &format!("Notiz konnte nicht aktualisiert werden: {error}"),
                    ),
                }
                save_button_for_result.set_sensitive(true);
            });
        });

        let selected_memo_for_export = Rc::clone(&selected_memo);
        export_memo_btn.connect_clicked(move |button| {
            let Some(memo) = selected_memo_for_export.borrow().clone() else {
                show_toast_for_widget(button, "Keine Notiz ausgewählt");
                return;
            };
            if memo.component.trim().is_empty() {
                show_toast_for_widget(
                    button,
                    "Für diese Notiz sind keine iCalendar-Daten verfügbar",
                );
                return;
            }
            let Some(root) = button.root() else {
                return;
            };
            let Ok(window) = root.downcast::<gtk::Window>() else {
                return;
            };
            let file_dialog = gtk::FileDialog::builder()
                .title("Notiz als iCalendar speichern")
                .initial_name("notiz.ics")
                .modal(true)
                .build();
            let component = memo.component;
            let button_for_result = button.clone();
            file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else {
                    return;
                };
                let save_result = file.replace_contents(
                    export_ical_component(&component).as_bytes(),
                    None,
                    true,
                    gio::FileCreateFlags::NONE,
                    None::<&gio::Cancellable>,
                );
                match save_result {
                    Ok(_) => {
                        show_toast_for_widget(&button_for_result, "Notiz als iCalendar exportiert")
                    }
                    Err(error) => show_toast_for_widget(
                        &button_for_result,
                        &format!("iCalendar konnte nicht gespeichert werden: {error}"),
                    ),
                }
            });
        });

        let selected_memo_for_delete = Rc::clone(&selected_memo);
        let memo_records_for_delete = Rc::clone(&memo_records);
        let page_weak_for_delete = self.downgrade();
        let memo_list_for_delete = memo_list.clone();
        let empty_state_for_delete = empty_state.clone();
        let memo_title_for_delete = memo_title.clone();
        let memo_text_for_delete = memo_text.clone();
        delete_memo_btn.connect_clicked(move |_| {
            let Some(memo) = selected_memo_for_delete.borrow().clone() else {
                show_toast_for_widget(&memo_title_for_delete, "Keine Notiz ausgewählt");
                return;
            };
            if memo.source_uid.is_empty() {
                show_toast_for_widget(
                    &memo_title_for_delete,
                    "Diese Notiz stammt aus einer schreibgeschützten importierten Quelle",
                );
                return;
            }
            let confirmation_message = if memo.source_uid == "local-calendar" {
                "Die Notiz wird dauerhaft aus der lokalen Notizliste entfernt."
            } else {
                "Die Notiz wird dauerhaft aus der EDS-Notizliste entfernt."
            };
            let memo_for_action = memo.clone();
            let selected_memo_for_action = Rc::clone(&selected_memo_for_delete);
            let memo_records_for_action = Rc::clone(&memo_records_for_delete);
            let page_weak_for_action = page_weak_for_delete.clone();
            let memo_list_for_action = memo_list_for_delete.clone();
            let empty_state_for_action = empty_state_for_delete.clone();
            let memo_title_for_action = memo_title_for_delete.clone();
            let memo_text_for_action = memo_text_for_delete.clone();
            crate::dialogs::run_destructive_action(
                &memo_title_for_delete,
                "Notiz löschen?",
                confirmation_message,
                "Löschen",
                move || {
                    let source_uid = memo_for_action.source_uid.clone();
                    let source_uid_for_result = source_uid.clone();
                    let uid = memo_for_action.uid.clone();
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = ECalService::new().remove_component(
                            &source_uid,
                            ECalComponentType::Memo,
                            &uid,
                        );
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Memo deletion result was dropped");
                        }
                    });

                    let memo_records_for_result = Rc::clone(&memo_records_for_action);
                    let selected_memo_for_result = Rc::clone(&selected_memo_for_action);
                    let memo_list_for_result = memo_list_for_action.clone();
                    let empty_state_for_result = empty_state_for_action.clone();
                    let memo_title_for_result = memo_title_for_action.clone();
                    let memo_text_for_result = memo_text_for_action.clone();
                    let page_weak_for_result = page_weak_for_action.clone();
                    glib::spawn_future_local(async move {
                        match receiver.recv().await {
                            Ok(Ok(result)) => {
                                let uid = result.uid;
                                remove_memo_row(
                                    &memo_list_for_result,
                                    Rc::clone(&memo_records_for_result),
                                    &source_uid_for_result,
                                    &uid,
                                );
                                *selected_memo_for_result.borrow_mut() = None;
                                memo_title_for_result.set_text("");
                                memo_text_for_result
                                    .buffer()
                                    .set_text("Noch keine Notiz ausgewählt.");
                                empty_state_for_result
                                    .set_visible(memo_records_for_result.borrow().is_empty());
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    page.show_toast(&format!("Notiz gelöscht ({uid})"));
                                }
                            }
                            Ok(Err(error)) => show_toast_for_widget(
                                &memo_title_for_result,
                                &format!("Notiz konnte nicht gelöscht werden: {error}"),
                            ),
                            Err(error) => tracing::debug!(%error, "Memo deletion channel closed"),
                        }
                    });
                },
            );
        });

        let memo_records_for_selection = Rc::clone(&memo_records);
        let selected_memo_for_selection = Rc::clone(&selected_memo);
        memo_list.connect_row_selected(glib::clone!(
            #[weak]
            memo_title,
            #[weak]
            memo_meta,
            #[weak]
            memo_text,
            #[strong]
            selected_memo_for_selection,
            move |_, row| {
                let Some(row) = row else {
                    *selected_memo_for_selection.borrow_mut() = None;
                    return;
                };
                let (source_uid, uid) = unsafe {
                    let source_uid = row
                        .data::<String>("memo-source-uid")
                        .map(|value| value.as_ref().clone())
                        .unwrap_or_default();
                    let uid = row
                        .data::<String>("memo-id")
                        .map(|value| value.as_ref().clone())
                        .unwrap_or_default();
                    (source_uid, uid)
                };
                let Some(memo) = memo_records_for_selection
                    .borrow()
                    .iter()
                    .find(|memo| memo.source_uid == source_uid && memo.uid == uid)
                    .cloned()
                else {
                    return;
                };
                *selected_memo_for_selection.borrow_mut() = Some(memo.clone());
                memo_title.set_text(&memo.summary);
                memo_meta.set_label(&format!("VJOURNAL · {}", memo.updated));
                memo_text.buffer().set_text(&memo.description);
            }
        ));

        let memo_list_for_load = memo_list.clone();
        let empty_state_for_load = empty_state.clone();
        let memo_meta_for_load = memo_meta.clone();
        let memo_records_for_load = Rc::clone(&memo_records);
        let (memo_sender, memo_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = load_snapshot_prefer_eds();
            if let Err(error) = memo_sender.send_blocking(result) {
                tracing::debug!(%error, "Memo snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match memo_receiver.recv().await {
                Ok(result) => {
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Memo data loaded with a backend warning");
                    }
                    if result.snapshot.memos.is_empty() {
                        empty_state_for_load.set_label(match result.origin {
                            CalendarDataOrigin::Eds => "Keine VJOURNAL-Daten aus EDS gefunden",
                            CalendarDataOrigin::Local => "Keine lokalen VJOURNAL-Daten gefunden",
                        });
                    } else {
                        empty_state_for_load.set_visible(false);
                        let origin = match result.origin {
                            CalendarDataOrigin::Eds => "EDS",
                            CalendarDataOrigin::Local => "lokalem iCalendar",
                        };
                        memo_meta_for_load.set_label(&format!(
                            "{} Notiz(en) aus {origin} geladen",
                            result.snapshot.memos.len()
                        ));
                        for memo in result.snapshot.memos {
                            upsert_memo_row(
                                &memo_list_for_load,
                                Rc::clone(&memo_records_for_load),
                                memo,
                            );
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "Memo snapshot unavailable"),
            }
        });

        if let Some((cancel, receiver)) = ECalService::new().start_change_monitor() {
            *self.imp().ecal_monitor_cancel.borrow_mut() = Some(cancel);
            let memo_list_for_monitor = memo_list.clone();
            let memo_records_for_monitor = Rc::clone(&memo_records);
            let empty_state_for_monitor = empty_state.clone();
            let selected_memo_for_monitor = Rc::clone(&selected_memo);
            let memo_title_for_monitor = memo_title.clone();
            let memo_meta_for_monitor = memo_meta.clone();
            let memo_text_for_monitor = memo_text.clone();
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
                                    "ECal memo signal monitor started with source warnings"
                                );
                            }
                        }
                        ECalSyncEvent::Change(change) => match change.kind {
                            ECalChangeKind::Removed => {
                                let removed = remove_memo_row(
                                    &memo_list_for_monitor,
                                    Rc::clone(&memo_records_for_monitor),
                                    &change.source_uid,
                                    &change.uid,
                                );
                                if removed
                                    && selected_memo_for_monitor.borrow().as_ref().is_some_and(
                                        |memo| {
                                            memo.source_uid == change.source_uid
                                                && memo.uid == change.uid
                                        },
                                    )
                                {
                                    *selected_memo_for_monitor.borrow_mut() = None;
                                    memo_title_for_monitor.set_text("");
                                    memo_meta_for_monitor.set_label("Keine Notiz ausgewählt");
                                    memo_text_for_monitor
                                        .buffer()
                                        .set_text("Noch keine Notizdaten verfügbar.");
                                }
                            }
                            ECalChangeKind::Added | ECalChangeKind::Modified => {
                                if let Some(ECalRecord::Memo(memo)) = parse_change_record(&change) {
                                    upsert_memo_row(
                                        &memo_list_for_monitor,
                                        Rc::clone(&memo_records_for_monitor),
                                        memo.clone(),
                                    );
                                    if selected_memo_for_monitor.borrow().as_ref().is_some_and(
                                        |selected| {
                                            selected.source_uid == memo.source_uid
                                                && selected.uid == memo.uid
                                        },
                                    ) {
                                        *selected_memo_for_monitor.borrow_mut() =
                                            Some(memo.clone());
                                        memo_title_for_monitor.set_text(&memo.summary);
                                        memo_meta_for_monitor
                                            .set_label(&format!("VJOURNAL · {}", memo.updated));
                                        memo_text_for_monitor.buffer().set_text(&memo.description);
                                    }
                                }
                            }
                        },
                        ECalSyncEvent::Error(error) => {
                            tracing::warn!(%error, "ECal memo signal monitor stopped");
                            break;
                        }
                    }
                    empty_state_for_monitor
                        .set_visible(memo_records_for_monitor.borrow().is_empty());
                }
            });
        }

        let content_page = adw::NavigationPage::builder()
            .title("Notiz-Details")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        split.set_min_sidebar_width(260.0);
        split.set_max_sidebar_width(360.0);
        split.set_parent(self);
    }

    fn open_memo_dialog(
        &self,
        memo_list: gtk::ListBox,
        memo_records: Rc<RefCell<Vec<MemoRecord>>>,
        empty_state: gtk::Label,
    ) {
        let dialog = adw::Window::builder()
            .title("Neue Notiz")
            .modal(true)
            .default_width(560)
            .default_height(540)
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

        let source_model = gtk::StringList::new(&["EDS-Notizlisten werden geladen…"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_row = adw::ComboRow::builder()
            .title("Notizliste")
            .model(&source_model)
            .build();
        let settings_for_default_memo = AppSettings::try_new();
        let source_uids_for_selection = Rc::clone(&source_uids);
        source_row.connect_selected_notify(move |row| {
            let Some(settings) = settings_for_default_memo.as_ref() else {
                return;
            };
            let Some(source_uid) = source_uids_for_selection
                .borrow()
                .get(row.selected() as usize)
                .cloned()
            else {
                return;
            };
            settings.set_default_memo_source_uid(&source_uid);
        });
        let details = adw::PreferencesGroup::builder().title("Details").build();
        details.add(&source_row);
        let title_entry = adw::EntryRow::builder().title("Titel").build();
        details.add(&title_entry);
        content_box.append(&details);

        let description_group = adw::PreferencesGroup::builder().title("Notiz").build();
        let description_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(260)
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

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let local_calendar_available = CalendarStoreService::new().is_writable();
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_memo_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        glib::spawn_future_local(async move {
            match eds_service::list_calendar_sources().await {
                Ok(sources) => {
                    let active_sources: Vec<_> = sources
                        .into_iter()
                        .filter(|source| source.enabled)
                        .collect();
                    let mut names: Vec<String> = active_sources
                        .iter()
                        .map(|source| {
                            if source.backend_name.is_empty() {
                                source.display_name.clone()
                            } else {
                                format!("{} · {}", source.display_name, source.backend_name)
                            }
                        })
                        .collect();
                    let mut uids = source_uids_for_load.borrow_mut();
                    uids.clear();
                    uids.extend(active_sources.into_iter().map(|source| source.uid));
                    if local_calendar_available {
                        names.push("Lokale Notizliste".to_string());
                        uids.push("local-calendar".to_string());
                    }
                    if names.is_empty() {
                        names.push("Keine aktive EDS-Notizliste verfügbar".to_string());
                    }
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
                    let selected_index = preferred_source_uid
                        .as_deref()
                        .and_then(|uid| uids.iter().position(|candidate| candidate == uid));
                    drop(uids);
                    if let Some(index) = selected_index {
                        source_row_for_load.set_selected(index as u32);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS memo lists for memo dialog");
                    let mut names = Vec::new();
                    let mut uids = source_uids_for_load.borrow_mut();
                    uids.clear();
                    if local_calendar_available {
                        names.push("Lokale Notizliste".to_string());
                        uids.push("local-calendar".to_string());
                    } else {
                        names.push("EDS-Notizlisten sind nicht erreichbar".to_string());
                    }
                    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
                }
            }
        });

        let dialog_weak = dialog.downgrade();
        let page_weak = self.downgrade();
        let source_uids_for_save = Rc::clone(&source_uids);
        save_button.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let selected = source_row.selected() as usize;
            let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() else {
                show_toast_for_widget(&dialog, "Keine aktive EDS-Notizliste ist ausgewählt");
                return;
            };
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let component = match build_memo_component(&title_entry.text(), &description) {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&dialog, &error);
                    return;
                }
            };
            let component_for_model = component.clone();
            let title_for_model = title_entry.text().trim().to_string();
            let description_for_model = description.to_string();
            let source_uid_for_model = source_uid.clone();
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = ECalService::new().create_component(
                    &source_uid,
                    ECalComponentType::Memo,
                    &component,
                );
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Memo mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog_weak.clone();
            let page_weak_for_result = page_weak.clone();
            let memo_list_for_result = memo_list.clone();
            let memo_records_for_result = Rc::clone(&memo_records);
            let empty_state_for_result = empty_state.clone();
            glib::spawn_future_local(async move {
                let Ok(result) = receiver.recv().await else {
                    return;
                };
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    return;
                };
                match result {
                    Ok(result) => {
                        let component = result.component.unwrap_or(component_for_model);
                        let uid = result.uid;
                        let source_display_name = if source_uid_for_model == "local-calendar" {
                            "Lokale Notizliste".to_string()
                        } else {
                            "EDS".to_string()
                        };
                        let memo = MemoRecord {
                            uid: uid.clone(),
                            source_uid: source_uid_for_model,
                            source_display_name,
                            component,
                            summary: title_for_model,
                            description: description_for_model,
                            updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                        };
                        upsert_memo_row(
                            &memo_list_for_result,
                            Rc::clone(&memo_records_for_result),
                            memo,
                        );
                        empty_state_for_result.set_visible(false);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            page.show_toast(&format!("Notiz gespeichert ({uid})"));
                        }
                        dialog.close();
                    }
                    Err(error) => show_toast_for_widget(
                        &dialog,
                        &format!("Notiz konnte nicht gespeichert werden: {error}"),
                    ),
                }
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
        tracing::warn!(%message, "Memo page result");
    }
}

fn upsert_memo_row(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    memo: MemoRecord,
) {
    let existing_index = memo_records
        .borrow()
        .iter()
        .position(|existing| existing.source_uid == memo.source_uid && existing.uid == memo.uid);
    if let Some(index) = existing_index {
        memo_records.borrow_mut().remove(index);
        remove_memo_list_row(memo_list, &memo.source_uid, &memo.uid);
    }
    memo_records.borrow_mut().push(memo.clone());
    append_memo_row(memo_list, &memo);
}

fn append_memo_row(memo_list: &gtk::ListBox, memo: &MemoRecord) {
    let row = gtk::ListBoxRow::new();
    unsafe {
        row.set_data("memo-id", memo.uid.clone());
        row.set_data("memo-source-uid", memo.source_uid.clone());
        row.set_data("memo-search", memo_search_text(memo));
        row.set_data("memo-sort-updated", memo.updated.to_lowercase());
        row.set_data("memo-sort-title", memo.summary.to_lowercase());
        row.set_data("memo-sort-source", memo.source_display_name.to_lowercase());
    }
    let title = glib::markup_escape_text(&memo.summary);
    let action_row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(&memo.updated)
        .build();
    if let Some(color) = CalendarCategoryStore::default().color_for_component(&memo.component) {
        let category_dot = gtk::Label::new(None);
        category_dot.set_markup(&format!("<span foreground=\"{color}\">●</span>"));
        category_dot.set_tooltip_text(Some("Notizkategorie"));
        action_row.add_prefix(&category_dot);
    }
    row.set_child(Some(&action_row));
    memo_list.append(&row);
}

fn memo_search_text(memo: &MemoRecord) -> String {
    format!(
        "{} {} {} {} {}",
        memo.summary, memo.description, memo.updated, memo.source_display_name, memo.component
    )
    .to_lowercase()
}

fn compare_memo_rows(
    left: &gtk::ListBoxRow,
    right: &gtk::ListBoxRow,
    sort: MemoSort,
    descending: bool,
) -> std::cmp::Ordering {
    let data = |row: &gtk::ListBoxRow, key: &str| unsafe {
        row.data::<String>(key)
            .map(|value| value.as_ref().clone())
            .unwrap_or_default()
    };
    let ordering = match sort {
        MemoSort::Updated => data(left, "memo-sort-updated").cmp(&data(right, "memo-sort-updated")),
        MemoSort::Title => data(left, "memo-sort-title").cmp(&data(right, "memo-sort-title")),
        MemoSort::Source => data(left, "memo-sort-source").cmp(&data(right, "memo-sort-source")),
    };
    let ordering = if descending {
        ordering.reverse()
    } else {
        ordering
    };
    let source_order = data(left, "memo-source-uid").cmp(&data(right, "memo-source-uid"));
    let source_order = if descending {
        source_order.reverse()
    } else {
        source_order
    };
    let id_order = data(left, "memo-id").cmp(&data(right, "memo-id"));
    let id_order = if descending {
        id_order.reverse()
    } else {
        id_order
    };
    ordering.then(source_order).then(id_order)
}

fn remove_memo_list_row(memo_list: &gtk::ListBox, source_uid: &str, uid: &str) {
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            let row_source_uid = row
                .data::<String>("memo-source-uid")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            let row_uid = row
                .data::<String>("memo-id")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default();
            row_source_uid == source_uid && row_uid == uid
        };
        if matches {
            memo_list.remove(&row);
            return;
        }
    }
}

fn remove_memo_row(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    source_uid: &str,
    uid: &str,
) -> bool {
    let Some(index) = memo_records
        .borrow()
        .iter()
        .position(|memo| memo.source_uid == source_uid && memo.uid == uid)
    else {
        return false;
    };
    memo_records.borrow_mut().remove(index);
    remove_memo_list_row(memo_list, source_uid, uid);
    true
}

fn show_toast_for_widget<W: IsA<gtk::Widget>>(widget: &W, message: &str) {
    if let Some(root) = widget.root() {
        if let Ok(window) = root.downcast::<crate::window::EvolutionWindow>() {
            window.add_toast(&adw::Toast::new(message));
            return;
        }
    }
    tracing::warn!(%message, "Memo UI result");
}

fn build_memo_component(title: &str, description: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Eine Notiz benötigt einen Titel".to_string());
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut component = vec![
        "BEGIN:VJOURNAL".to_string(),
        format!("UID:evolution-gtk4-{}", uuid::Uuid::new_v4()),
        format!("DTSTAMP:{stamp}"),
        format!("DTSTART:{stamp}"),
        format!("SUMMARY:{}", escape_ical_text(title)),
    ];
    if !description.trim().is_empty() {
        component.push(format!(
            "DESCRIPTION:{}",
            escape_ical_text(description.trim())
        ));
    }
    component.push("END:VJOURNAL".to_string());
    Ok(format!("{}\r\n", component.join("\r\n")))
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
    fn memo_component_serialization_is_ical_safe() {
        let component = build_memo_component("A, note; title", "Line one\nLine two")
            .expect("memo component should be valid");
        assert!(component.starts_with("BEGIN:VJOURNAL\r\n"));
        assert!(component.contains("SUMMARY:A\\, note\\; title\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.ends_with("END:VJOURNAL\r\n"));
    }

    #[test]
    fn memo_component_rejects_empty_title() {
        assert!(build_memo_component("  ", "body").is_err());
    }

    #[test]
    fn memo_search_text_includes_title_description_and_component() {
        let memo = MemoRecord {
            uid: "memo-1".to_string(),
            source_uid: "local-calendar".to_string(),
            source_display_name: "Lokal".to_string(),
            component: "CATEGORIES:Project-X".to_string(),
            summary: "Release notes".to_string(),
            description: "Remember the migration".to_string(),
            updated: "2026-08-11 10:00".to_string(),
        };
        let search = memo_search_text(&memo);
        assert!(search.contains("release notes"));
        assert!(search.contains("migration"));
        assert!(search.contains("project-x"));
    }
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
