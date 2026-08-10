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
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::calendar_store_service::MemoRecord;
use crate::services::calendar_store_service::update_ical_property;
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalComponentType, ECalService, load_snapshot_prefer_eds,
};
use crate::services::eds_service;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MemosPage;

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

impl MemosPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::new();

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
            .margin_bottom(4)
            .build();
        sidebar_box.append(&search_entry);

        let memo_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

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
        memo_actions.append(&save_memo_btn);
        memo_actions.append(&delete_memo_btn);
        content_box.append(&memo_actions);
        content_box.append(&memo_meta);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content_box.append(&memo_text);

        let memo_records = Rc::new(RefCell::new(Vec::<MemoRecord>::new()));
        let selected_memo = Rc::new(RefCell::new(None::<MemoRecord>));

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
        let save_memo_btn_for_signal = save_memo_btn.clone();
        save_memo_btn_for_signal.connect_clicked(move |_| {
            let Some(memo) = selected_memo_for_save.borrow().clone() else {
                show_toast_for_widget(&memo_title_for_save, "Keine Notiz ausgewählt");
                return;
            };
            if memo.source_uid.is_empty() {
                show_toast_for_widget(
                    &memo_title_for_save,
                    "Lokale Notizen können ohne EDS-Quelle nicht geändert werden",
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
                        if let Some(existing) = memo_records_for_result
                            .borrow_mut()
                            .iter_mut()
                            .find(|existing| existing.uid == memo.uid)
                        {
                            *existing = updated.clone();
                        }
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
                    "Lokale Notizen können ohne EDS-Quelle nicht gelöscht werden",
                );
                return;
            }
            let confirmation = adw::AlertDialog::new(
                Some("Notiz löschen?"),
                Some("Die Notiz wird dauerhaft aus der EDS-Notizliste entfernt."),
            );
            confirmation.add_responses(&[("cancel", "Abbrechen"), ("delete", "Löschen")]);
            confirmation.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let confirmation_parent = memo_title_for_delete.clone();
            let selected_memo_for_response = Rc::clone(&selected_memo_for_delete);
            let memo_records_for_response = Rc::clone(&memo_records_for_delete);
            let page_weak_for_response = page_weak_for_delete.clone();
            let memo_list_for_response = memo_list_for_delete.clone();
            let empty_state_for_response = empty_state_for_delete.clone();
            let memo_title_for_response = memo_title_for_delete.clone();
            let memo_text_for_response = memo_text_for_delete.clone();
            confirmation.connect_response(
                None,
                move |dialog: &adw::AlertDialog, response: &str| {
                    if response != "delete" {
                        return;
                    }
                    let source_uid = memo.source_uid.clone();
                    let uid = memo.uid.clone();
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

                    let dialog_for_result = dialog.clone();
                    let memo_records_for_result = Rc::clone(&memo_records_for_response);
                    let selected_memo_for_result = Rc::clone(&selected_memo_for_response);
                    let memo_list_for_result = memo_list_for_response.clone();
                    let empty_state_for_result = empty_state_for_response.clone();
                    let memo_title_for_result = memo_title_for_response.clone();
                    let memo_text_for_result = memo_text_for_response.clone();
                    let page_weak_for_result = page_weak_for_response.clone();
                    glib::spawn_future_local(async move {
                        match receiver.recv().await {
                            Ok(Ok(result)) => {
                                let uid = result.uid;
                                if let Some(index) = memo_records_for_result
                                    .borrow()
                                    .iter()
                                    .position(|memo| memo.uid == uid)
                                {
                                    if let Some(row) =
                                        memo_list_for_result.row_at_index(index as i32)
                                    {
                                        memo_list_for_result.remove(&row);
                                    }
                                    memo_records_for_result.borrow_mut().remove(index);
                                }
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
                                dialog_for_result.close();
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
            confirmation.present(Some(&confirmation_parent));
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
                let index = row.index();
                let Some(memo) = memo_records_for_selection
                    .borrow()
                    .get(index as usize)
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
                        let mut records = memo_records_for_load.borrow_mut();
                        for memo in result.snapshot.memos {
                            records.push(memo.clone());
                            let row = adw::ActionRow::builder()
                                .title(&memo.summary)
                                .subtitle(&memo.updated)
                                .build();
                            memo_list_for_load.append(&row);
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "Memo snapshot unavailable"),
            }
        });

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
                    if uids.is_empty() {
                        source_model_for_load.splice(
                            0,
                            source_model_for_load.n_items(),
                            &["Keine aktive EDS-Notizliste verfügbar"],
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load EDS memo lists for memo dialog");
                    source_model_for_load.splice(
                        0,
                        source_model_for_load.n_items(),
                        &["EDS-Notizlisten sind nicht erreichbar"],
                    );
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
                        let memo = MemoRecord {
                            uid: uid.clone(),
                            source_uid: source_uid_for_model,
                            source_display_name: "EDS".to_string(),
                            component,
                            summary: title_for_model,
                            description: description_for_model,
                            updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                        };
                        memo_records_for_result.borrow_mut().push(memo.clone());
                        let row = adw::ActionRow::builder()
                            .title(&memo.summary)
                            .subtitle(&memo.updated)
                            .build();
                        memo_list_for_result.append(&row);
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
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
