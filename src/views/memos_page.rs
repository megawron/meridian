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

use crate::dialogs::event_dialog::AlarmEditor;
use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarStoreService, MemoAttachment, MemoEditorDraft, MemoRecord, export_ical_component,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService, ECalSyncEvent,
    load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::eds_service;
use crate::services::reminder_service::{Alarm, replace_component_alarms};
use crate::services::settings_service::AppSettings;
use crate::views::accessibility::set_accessible_label;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct MemosPage {
        pub ecal_monitor_cancel: RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
        pub memo_category_filter: RefCell<Option<String>>,
        pub memo_category_model: RefCell<Option<gtk::StringList>>,
        pub memo_category_selector: RefCell<Option<gtk::DropDown>>,
        pub memo_source_filter: RefCell<Option<String>>,
        pub memo_search_query: RefCell<String>,
        pub memo_list: RefCell<Option<gtk::ListBox>>,
        pub memo_records: RefCell<Option<Rc<RefCell<Vec<MemoRecord>>>>>,
        pub memo_empty_state: RefCell<Option<gtk::Label>>,
        pub new_memo_button: RefCell<Option<gtk::Button>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
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

    impl WidgetImpl for MemosPage {
        fn realize(&self) {
            self.parent_realize();
        }
    }
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

    /// Trigger the page-local memo editor from the shared Evolution toolbar.
    pub fn present_new_memo(&self) {
        if let Some(button) = self.imp().new_memo_button.borrow().as_ref() {
            button.activate();
        }
    }

    pub(crate) fn focus_search(&self) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    pub(crate) fn set_search_query(&self, query: &str) {
        if let Some(entry) = self.imp().search_entry.borrow().as_ref() {
            entry.set_text(query);
            entry.grab_focus();
        }
    }

    pub(crate) fn select_search_result(&self, source_uid: &str, uid: &str) -> bool {
        let Some(memo_list) = self.imp().memo_list.borrow().as_ref().cloned() else {
            return false;
        };
        select_memo_row(&memo_list, source_uid, uid)
    }

    /// Rebuild memo rows so category colors and the category filter reflect
    /// the presentation-only category store immediately.
    pub fn refresh_category_presentation(&self) {
        let (Some(memo_list), Some(memo_records), Some(empty_state)) = (
            self.imp().memo_list.borrow().as_ref().cloned(),
            self.imp().memo_records.borrow().as_ref().cloned(),
            self.imp().memo_empty_state.borrow().as_ref().cloned(),
        ) else {
            return;
        };

        let selected_identity = memo_list.selected_row().and_then(|row| unsafe {
            let uid = row.data::<String>("memo-id")?.as_ref().clone();
            let source_uid = row.data::<String>("memo-source-uid")?.as_ref().clone();
            Some((source_uid, uid))
        });
        while let Some(child) = memo_list.first_child() {
            memo_list.remove(&child);
        }
        for memo in memo_records.borrow().iter() {
            append_memo_row(&memo_list, memo);
        }

        refresh_memo_category_model(self, &memo_list);
        let source_filter = self.imp().memo_source_filter.borrow().clone();
        let category_filter = self.imp().memo_category_filter.borrow().clone();
        let query = self.imp().memo_search_query.borrow().clone();
        apply_memo_list_filter(
            &memo_list,
            source_filter.clone(),
            category_filter.clone(),
            query.clone(),
        );
        let has_records = memo_records.borrow().iter().any(|memo| {
            memo_matches_filter(
                memo,
                source_filter.as_deref(),
                category_filter.as_deref(),
                &query,
            )
        });
        empty_state.set_label(
            if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                "Keine synchronisierten Notizen geladen"
            } else {
                "Keine passenden Notizen"
            },
        );
        empty_state.set_visible(!has_records);

        if let Some((source_uid, uid)) = selected_identity {
            let mut child = memo_list.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                    continue;
                };
                let matches = unsafe {
                    row.data::<String>("memo-id")
                        .is_some_and(|value| value.as_ref() == &uid)
                        && row
                            .data::<String>("memo-source-uid")
                            .is_some_and(|value| value.as_ref() == &source_uid)
                };
                if matches {
                    memo_list.select_row(Some(&row));
                    break;
                }
            }
        }
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(240.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.0)
            .build();

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
        let memo_source_filter = Rc::new(RefCell::new(
            AppSettings::try_new()
                .map(|settings| settings.memo_source_filter())
                .filter(|source_uid| !source_uid.trim().is_empty()),
        ));
        self.imp()
            .memo_source_filter
            .replace(memo_source_filter.borrow().clone());

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
        self.imp().search_entry.replace(Some(search_entry.clone()));
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
            set_accessible_label(
                button,
                if descending {
                    "Sortierung aufsteigend ändern"
                } else {
                    "Sortierung absteigend ändern"
                },
            );
        };
        set_direction_button(&direction_button, memo_sort_descending.get());
        let source_model = gtk::StringList::new(&["Alle Notizlisten"]);
        let source_uids = Rc::new(RefCell::new(Vec::<String>::new()));
        let source_selector =
            gtk::DropDown::new(Some(source_model.clone()), None::<&gtk::Expression>);
        source_selector.set_tooltip_text(Some("Notizliste auswählen"));
        let initial_source_filter = memo_source_filter.borrow().clone();
        let category_model = gtk::StringList::new(&["Alle Kategorien"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Notizkategorie auswählen"));
        self.imp()
            .memo_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .memo_category_selector
            .replace(Some(category_selector.clone()));
        search_entry.set_hexpand(true);
        sidebar_box.append(&search_entry);

        let sort_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_bottom(4)
            .build();
        sort_selector.set_hexpand(true);
        sort_row.append(&sort_selector);
        sort_row.append(&direction_button);
        sidebar_box.append(&sort_row);

        let memo_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();

        let source_header = gtk::Label::builder()
            .label("Notizlisten")
            .css_classes(vec!["title-5"])
            .halign(gtk::Align::Start)
            .margin_top(8)
            .margin_bottom(2)
            .build();
        sidebar_box.append(&source_header);
        let memo_source_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        sidebar_box.append(&memo_source_list);
        let source_selector_for_sidebar = source_selector.clone();
        memo_source_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                source_selector_for_sidebar.set_selected(row.index() as u32);
            }
        });

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
        let memo_mutation_generation = Rc::new(Cell::new(0u64));

        let empty_state = gtk::Label::builder()
            .label("Keine synchronisierten Notizen geladen")
            .css_classes(vec!["dim-label"])
            .wrap(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        self.imp().memo_list.replace(Some(memo_list.clone()));
        self.imp()
            .memo_records
            .replace(Some(Rc::clone(&memo_records)));
        self.imp()
            .memo_empty_state
            .replace(Some(empty_state.clone()));
        self.imp()
            .memo_search_query
            .replace(search_entry.text().to_string());
        sidebar_box.append(&empty_state);

        sidebar_box.append(&memo_list);

        let memo_records_for_search = Rc::clone(&memo_records);
        let memo_source_filter_for_search = Rc::clone(&memo_source_filter);
        let page_weak_for_search = self.downgrade();
        let empty_state_for_search = empty_state.clone();
        search_entry.connect_search_changed(glib::clone!(
            #[weak]
            memo_list,
            move |entry| {
                let query = entry.text().trim().to_lowercase();
                if let Some(page) = page_weak_for_search.upgrade() {
                    page.imp().memo_search_query.replace(query.clone());
                }
                let source_filter = memo_source_filter_for_search.borrow().clone();
                if let Some(page) = page_weak_for_search.upgrade() {
                    refresh_memo_category_model(&page, &memo_list);
                }
                let category_filter = page_weak_for_search
                    .upgrade()
                    .and_then(|page| page.imp().memo_category_filter.borrow().clone());
                apply_memo_list_filter(
                    &memo_list,
                    source_filter.clone(),
                    category_filter.clone(),
                    query.clone(),
                );
                empty_state_for_search.set_label(
                    if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                        "Keine synchronisierten Notizen geladen"
                    } else {
                        "Keine passenden Notizen"
                    },
                );
                let has_records = memo_records_for_search.borrow().iter().any(|memo| {
                    memo_matches_filter(
                        memo,
                        source_filter.as_deref(),
                        category_filter.as_deref(),
                        &query,
                    )
                });
                empty_state_for_search.set_visible(!has_records);
            }
        ));

        let memo_source_filter_for_selector = Rc::clone(&memo_source_filter);
        let source_uids_for_selector = Rc::clone(&source_uids);
        let memo_source_list_for_selector = memo_source_list.clone();
        let search_entry_for_source = search_entry.clone();
        let memo_list_for_source = memo_list.clone();
        let memo_records_for_source = Rc::clone(&memo_records);
        let page_weak_for_source = self.downgrade();
        let empty_state_for_source = empty_state.clone();
        source_selector.connect_selected_notify(move |selector| {
            let source_filter = if selector.selected() == 0 {
                None
            } else {
                source_uids_for_selector
                    .borrow()
                    .get(selector.selected() as usize - 1)
                    .cloned()
            };
            memo_source_filter_for_selector.replace(source_filter.clone());
            if let Some(page) = page_weak_for_source.upgrade() {
                page.imp().memo_source_filter.replace(source_filter.clone());
            }
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_source_filter(source_filter.as_deref().unwrap_or_default());
            }
            if let Some(row) =
                memo_source_list_for_selector.row_at_index(selector.selected() as i32)
            {
                memo_source_list_for_selector.select_row(Some(&row));
            }
            let query = search_entry_for_source.text().trim().to_lowercase();
            if let Some(page) = page_weak_for_source.upgrade() {
                refresh_memo_category_model(&page, &memo_list_for_source);
            }
            let category_filter = page_weak_for_source
                .upgrade()
                .and_then(|page| page.imp().memo_category_filter.borrow().clone());
            apply_memo_list_filter(
                &memo_list_for_source,
                source_filter.clone(),
                category_filter.clone(),
                query.clone(),
            );
            empty_state_for_source.set_label(
                if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                    "Keine synchronisierten Notizen geladen"
                } else {
                    "Keine passenden Notizen"
                },
            );
            let has_records = memo_records_for_source.borrow().iter().any(|memo| {
                memo_matches_filter(
                    memo,
                    source_filter.as_deref(),
                    category_filter.as_deref(),
                    &query,
                )
            });
            empty_state_for_source.set_visible(!has_records);
        });

        let page_weak_for_category = self.downgrade();
        let category_model_for_selector = category_model.clone();
        let memo_source_filter_for_category = Rc::clone(&memo_source_filter);
        let search_entry_for_category = search_entry.clone();
        let memo_list_for_category = memo_list.clone();
        let memo_records_for_category = Rc::clone(&memo_records);
        let empty_state_for_category = empty_state.clone();
        category_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_category.upgrade() else {
                return;
            };
            let category = if selector.selected() == 0 {
                None
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
            };
            page.imp().memo_category_filter.replace(category.clone());
            let source_filter = memo_source_filter_for_category.borrow().clone();
            let query = search_entry_for_category.text().trim().to_lowercase();
            apply_memo_list_filter(
                &memo_list_for_category,
                source_filter.clone(),
                category.clone(),
                query.clone(),
            );
            empty_state_for_category.set_label(
                if query.is_empty() && source_filter.is_none() && category.is_none() {
                    "Keine synchronisierten Notizen geladen"
                } else {
                    "Keine passenden Notizen"
                },
            );
            let has_records = memo_records_for_category.borrow().iter().any(|memo| {
                memo_matches_filter(memo, source_filter.as_deref(), category.as_deref(), &query)
            });
            empty_state_for_category.set_visible(!has_records);
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_selector_for_load = source_selector.clone();
        let memo_source_list_for_load = memo_source_list.clone();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            let mut sources = discovery.sources;
            sources.retain(|source| source.enabled);
            sources.sort_by_cached_key(|source| {
                (source.display_name.to_lowercase(), source.uid.clone())
            });

            let mut names = vec!["Alle Notizlisten".to_string()];
            let mut uids = Vec::new();
            for source in sources {
                names.push(if source.backend_name.trim().is_empty() {
                    source.display_name
                } else {
                    format!("{} · {}", source.display_name, source.backend_name)
                });
                uids.push(source.uid);
            }
            if let Some(initial_source_filter) = initial_source_filter.as_deref()
                && !uids.iter().any(|uid| uid == initial_source_filter)
            {
                names.push(format!("Nicht verfügbare Quelle · {initial_source_filter}"));
                uids.push(initial_source_filter.to_string());
            }
            if CalendarStoreService::new().is_writable() {
                names.push("Lokale Notizliste".to_string());
                uids.push("local-calendar".to_string());
            }

            let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            while let Some(child) = memo_source_list_for_load.first_child() {
                memo_source_list_for_load.remove(&child);
            }
            for (index, name) in names.iter().enumerate() {
                let escaped = glib::markup_escape_text(name);
                let row = adw::ActionRow::builder().title(escaped.as_str()).build();
                let image = gtk::Image::from_icon_name(if index == 0 {
                    "view-list-symbolic"
                } else {
                    "view-list-details-symbolic"
                });
                row.add_prefix(&image);
                memo_source_list_for_load.append(&row);
            }
            let selected = initial_source_filter
                .as_deref()
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
                .map(|index| index as u32 + 1)
                .unwrap_or(0);
            source_selector_for_load.set_selected(selected);
            if let Some(row) = memo_source_list_for_load.row_at_index(selected as i32) {
                memo_source_list_for_load.select_row(Some(&row));
            }
        });

        if let Some(settings) = AppSettings::try_new() {
            let memo_sort_for_settings = Rc::clone(&memo_sort);
            let sort_selector_for_settings = sort_selector.clone();
            let memo_list_for_sort_settings = memo_list.clone();
            settings.connect_changed(Some("memo-sort-field"), move |settings, _| {
                let sort = MemoSort::from_setting(settings.string("memo-sort-field").as_str());
                memo_sort_for_settings.set(sort);
                sort_selector_for_settings.set_selected(match sort {
                    MemoSort::Updated => 0,
                    MemoSort::Title => 1,
                    MemoSort::Source => 2,
                });
                memo_list_for_sort_settings.invalidate_sort();
            });

            let memo_sort_descending_for_settings = Rc::clone(&memo_sort_descending);
            let direction_button_for_settings = direction_button.clone();
            let memo_list_for_direction_settings = memo_list.clone();
            settings.connect_changed(Some("memo-sort-descending"), move |settings, _| {
                let descending = settings.boolean("memo-sort-descending");
                memo_sort_descending_for_settings.set(descending);
                direction_button_for_settings.set_icon_name(if descending {
                    "view-sort-descending-symbolic"
                } else {
                    "view-sort-ascending-symbolic"
                });
                direction_button_for_settings.set_tooltip_text(Some(if descending {
                    "Absteigend sortiert"
                } else {
                    "Aufsteigend sortiert"
                }));
                set_accessible_label(
                    &direction_button_for_settings,
                    if descending {
                        "Sortierung aufsteigend ändern"
                    } else {
                        "Sortierung absteigend ändern"
                    },
                );
                memo_list_for_direction_settings.invalidate_sort();
            });

            let memo_source_filter_for_settings = Rc::clone(&memo_source_filter);
            let source_uids_for_settings = Rc::clone(&source_uids);
            let source_selector_for_settings = source_selector.clone();
            let memo_list_for_source_settings = memo_list.clone();
            let memo_records_for_source_settings = Rc::clone(&memo_records);
            let empty_state_for_source_settings = empty_state.clone();
            let search_entry_for_source_settings = search_entry.clone();
            let page_weak_for_source_settings = self.downgrade();
            settings.connect_changed(Some("memo-source-filter"), move |settings, _| {
                let value = settings.string("memo-source-filter");
                let value = value.trim();
                memo_source_filter_for_settings
                    .replace((!value.is_empty()).then_some(value.to_string()));
                if let Some(page) = page_weak_for_source_settings.upgrade() {
                    page.imp()
                        .memo_source_filter
                        .replace((!value.is_empty()).then_some(value.to_string()));
                }
                let selected = if value.is_empty() {
                    Some(0)
                } else {
                    source_uids_for_settings
                        .borrow()
                        .iter()
                        .position(|uid| uid == value)
                        .map(|index| index as u32 + 1)
                };
                if let Some(selected) = selected {
                    source_selector_for_settings.set_selected(selected);
                    return;
                }

                let source_filter = (!value.is_empty()).then_some(value.to_string());
                let query = search_entry_for_source_settings
                    .text()
                    .trim()
                    .to_lowercase();
                let category_filter = page_weak_for_source_settings
                    .upgrade()
                    .and_then(|page| page.imp().memo_category_filter.borrow().clone());
                apply_memo_list_filter(
                    &memo_list_for_source_settings,
                    source_filter.clone(),
                    category_filter.clone(),
                    query.clone(),
                );
                let has_records = memo_records_for_source_settings
                    .borrow()
                    .iter()
                    .any(|memo| {
                        memo_matches_filter(
                            memo,
                            source_filter.as_deref(),
                            category_filter.as_deref(),
                            &query,
                        )
                    });
                empty_state_for_source_settings.set_label(
                    if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                        "Keine synchronisierten Notizen geladen"
                    } else {
                        "Keine passenden Notizen"
                    },
                );
                empty_state_for_source_settings.set_visible(!has_records);
            });
        }

        let new_memo_btn = gtk::Button::builder()
            .label("Neue Notiz")
            .icon_name("list-add-symbolic")
            .css_classes(vec!["suggested-action"])
            .margin_top(8)
            .build();
        self.imp()
            .new_memo_button
            .replace(Some(new_memo_btn.clone()));
        sidebar_box.append(&new_memo_btn);

        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&sidebar_box)
            .vexpand(true)
            .build();

        let drawer_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(vec!["sidebar-drawer-header"])
            .build();
        let drawer_icon = gtk::Image::from_icon_name("accessory-text-editor-symbolic");
        let drawer_title = gtk::Label::builder()
            .label("Notizen")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        drawer_header.append(&drawer_icon);
        drawer_header.append(&drawer_title);

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
            .build();
        sidebar_outer_box.append(&drawer_header);
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "memos",
            &sidebar_outer_box,
        ));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Notizen")
            .child(&sidebar_outer_box)
            .width_request(240)
            .css_classes(vec!["evolution-sidebar-drawer"])
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
            .label("Notizen werden nach der Synchronisation geladen")
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
        let memo_categories = adw::EntryRow::builder()
            .title("Kategorien (Komma getrennt)")
            .build();
        memo_categories.set_tooltip_text(Some("z. B. Arbeit, Privat"));
        content_box.append(&memo_categories);
        let memo_alarm_editor = AlarmEditor::new(None, "");
        content_box.append(memo_alarm_editor.widget());
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
                None,
            );
        });

        let save_memo_btn_for_context = save_memo_btn.clone();
        let export_memo_btn_for_context = export_memo_btn.clone();
        let delete_memo_btn_for_context = delete_memo_btn.clone();
        let selected_memo_for_save = Rc::clone(&selected_memo);
        let memo_records_for_save = Rc::clone(&memo_records);
        let page_weak_for_save = self.downgrade();
        let memo_title_for_save = memo_title.clone();
        let memo_categories_for_save = memo_categories.clone();
        let memo_text_for_save = memo_text.clone();
        let memo_alarm_editor_for_save = memo_alarm_editor.clone();
        let memo_list_for_save = memo_list.clone();
        let empty_state_for_save = empty_state.clone();
        save_memo_btn.connect_clicked(move |_| {
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
            let title = memo_title_for_save.text().to_string();
            let description = memo_text_for_save
                .buffer()
                .text(
                    &memo_text_for_save.buffer().start_iter(),
                    &memo_text_for_save.buffer().end_iter(),
                    false,
                )
                .to_string();
            let mut draft = match MemoEditorDraft::parse(&memo.component) {
                Ok(draft) => draft,
                Err(error) => {
                    show_toast_for_widget(&memo_title_for_save, &error);
                    return;
                }
            };
            draft.summary = title.clone();
            draft.description = description.clone();
            draft.categories = parse_categories_input(&memo_categories_for_save.text());
            memo_alarm_editor_for_save.set_summary(&title);
            draft.alarms = memo_alarm_editor_for_save.alarms();
            let component = match draft.apply() {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&memo_title_for_save, &error);
                    return;
                }
            };
            let staged = MemoRecord {
                uid: memo.uid,
                source_uid: memo.source_uid,
                source_display_name: memo.source_display_name,
                component,
                summary: title,
                description,
                updated: memo.updated,
            };
            let Some(page) = page_weak_for_save.upgrade() else {
                return;
            };
            page.open_memo_dialog(
                memo_list_for_save.clone(),
                Rc::clone(&memo_records_for_save),
                empty_state_for_save.clone(),
                Some(staged),
            );
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
            let component = match confirmed_memo_component(Some(memo.component)) {
                Ok((component, _)) => component,
                Err(error) => {
                    show_toast_for_widget(
                        button,
                        &format!(
                            "Diese Notiz enthält keine vollständige iCalendar-Komponente: {error}"
                        ),
                    );
                    return;
                }
            };
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
        let memo_categories_for_delete = memo_categories.clone();
        let memo_text_for_delete = memo_text.clone();
        let memo_mutation_generation_for_delete = Rc::clone(&memo_mutation_generation);
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
                "Die Notiz wird dauerhaft aus der synchronisierten Notizliste entfernt."
            };
            let delete_generation = memo_mutation_generation_for_delete.get();
            let memo_for_action = memo.clone();
            let selected_memo_for_action = Rc::clone(&selected_memo_for_delete);
            let memo_records_for_action = Rc::clone(&memo_records_for_delete);
            let page_weak_for_action = page_weak_for_delete.clone();
            let memo_list_for_action = memo_list_for_delete.clone();
            let empty_state_for_action = empty_state_for_delete.clone();
            let memo_title_for_action = memo_title_for_delete.clone();
            let memo_categories_for_action = memo_categories_for_delete.clone();
            let memo_text_for_action = memo_text_for_delete.clone();
            let memo_mutation_generation_for_action =
                Rc::clone(&memo_mutation_generation_for_delete);
            crate::dialogs::run_destructive_action(
                &memo_title_for_delete,
                "Notiz löschen?",
                confirmation_message,
                "Löschen",
                move || {
                    if memo_mutation_generation_for_action.get() != delete_generation {
                        return;
                    }
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
                    let memo_categories_for_result = memo_categories_for_action.clone();
                    let memo_text_for_result = memo_text_for_action.clone();
                    let page_weak_for_result = page_weak_for_action.clone();
                    let memo_mutation_generation_for_result =
                        Rc::clone(&memo_mutation_generation_for_action);
                    glib::spawn_future_local(async move {
                        let result = receiver.recv().await;
                        if memo_mutation_generation_for_result.get() != delete_generation {
                            return;
                        }
                        match result {
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
                                memo_categories_for_result.set_text("");
                                memo_text_for_result
                                    .buffer()
                                    .set_text("Noch keine Notiz ausgewählt.");
                                empty_state_for_result
                                    .set_visible(memo_records_for_result.borrow().is_empty());
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    refresh_memo_category_model(&page, &memo_list_for_result);
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

        // The original Evolution memo view exposes row actions through a
        // context menu as well as the detail toolbar.  Select the row under
        // the pointer first, then route the menu actions through the existing
        // save/export/delete buttons so all validation and ECal mutation code
        // remains centralized.
        let context_click = gtk::GestureClick::builder().button(3).build();
        let memo_list_weak_for_context = memo_list.downgrade();
        context_click.connect_pressed(move |_, _, x, y| {
            let Some(memo_list_for_context) = memo_list_weak_for_context.upgrade() else {
                return;
            };
            let Some(row) = memo_list_for_context.row_at_y(y as i32) else {
                return;
            };
            memo_list_for_context.select_row(Some(&row));

            let popover = gtk::Popover::builder()
                .has_arrow(false)
                .pointing_to(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1))
                .build();
            let menu_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(6)
                .margin_end(6)
                .margin_top(6)
                .margin_bottom(6)
                .build();

            let save = gtk::Button::with_label("Notiz speichern");
            save.set_halign(gtk::Align::Fill);
            save.add_css_class("flat");
            let popover_for_save = popover.clone();
            let save_button_for_menu = save_memo_btn_for_context.clone();
            save.connect_clicked(move |_| {
                popover_for_save.popdown();
                save_button_for_menu.activate();
            });
            menu_box.append(&save);

            let export = gtk::Button::with_label("Als iCalendar exportieren");
            export.set_halign(gtk::Align::Fill);
            export.add_css_class("flat");
            let popover_for_export = popover.clone();
            let export_button_for_menu = export_memo_btn_for_context.clone();
            export.connect_clicked(move |_| {
                popover_for_export.popdown();
                export_button_for_menu.activate();
            });
            menu_box.append(&export);

            let delete = gtk::Button::with_label("Notiz löschen");
            delete.set_halign(gtk::Align::Fill);
            delete.add_css_class("flat");
            delete.add_css_class("destructive-action");
            let popover_for_delete = popover.clone();
            let delete_button_for_menu = delete_memo_btn_for_context.clone();
            delete.connect_clicked(move |_| {
                popover_for_delete.popdown();
                delete_button_for_menu.activate();
            });
            menu_box.append(&delete);

            popover.set_child(Some(&menu_box));
            popover.set_parent(&memo_list_for_context);
            let popover_weak = popover.downgrade();
            popover.connect_closed(move |_| {
                if let Some(popover) = popover_weak.upgrade() {
                    popover.unparent();
                }
            });
            popover.popup();
        });
        memo_list.add_controller(context_click);

        let memo_records_for_selection = Rc::clone(&memo_records);
        let selected_memo_for_selection = Rc::clone(&selected_memo);
        let memo_mutation_generation_for_selection = Rc::clone(&memo_mutation_generation);
        let split_for_selection = split.clone();
        let memo_alarm_editor_for_selection = memo_alarm_editor.clone();
        memo_list.connect_row_selected(glib::clone!(
            #[weak]
            memo_title,
            #[weak]
            memo_categories,
            #[weak]
            memo_meta,
            #[weak]
            memo_text,
            #[strong]
            selected_memo_for_selection,
            #[strong]
            split_for_selection,
            #[strong]
            memo_mutation_generation_for_selection,
            #[strong]
            memo_alarm_editor_for_selection,
            move |_, row| {
                memo_mutation_generation_for_selection
                    .set(memo_mutation_generation_for_selection.get().wrapping_add(1));
                let Some(row) = row else {
                    *selected_memo_for_selection.borrow_mut() = None;
                    memo_alarm_editor_for_selection.set_component(None, "");
                    split_for_selection.set_show_content(false);
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
                memo_categories.set_text(&component_categories(&memo.component));
                memo_meta.set_label(&format!("VJOURNAL · {}", memo.updated));
                memo_text.buffer().set_text(&memo.description);
                memo_alarm_editor_for_selection.set_component(Some(&memo.component), &memo.summary);
                split_for_selection.set_show_content(true);
            }
        ));

        let memo_text_for_activate = memo_text.clone();
        memo_list.connect_row_activated(move |_, _| {
            memo_text_for_activate.grab_focus();
        });

        let memo_list_for_load = memo_list.clone();
        let empty_state_for_load = empty_state.clone();
        let memo_meta_for_load = memo_meta.clone();
        let memo_records_for_load = Rc::clone(&memo_records);
        let page_weak_for_load = self.downgrade();
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
                            CalendarDataOrigin::Eds => "Keine synchronisierten Notizen gefunden",
                            CalendarDataOrigin::Graph => {
                                "Keine VJOURNAL-Daten aus Microsoft Graph gefunden"
                            }
                            CalendarDataOrigin::Local => "Keine lokalen VJOURNAL-Daten gefunden",
                        });
                    } else {
                        empty_state_for_load.set_visible(false);
                        let origin = match result.origin {
                            CalendarDataOrigin::Eds => "synchronisiertem Notizbestand",
                            CalendarDataOrigin::Graph => "Microsoft Graph",
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
                    if let Some(page) = page_weak_for_load.upgrade() {
                        refresh_memo_category_model(&page, &memo_list_for_load);
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
            let memo_categories_for_monitor = memo_categories.clone();
            let memo_meta_for_monitor = memo_meta.clone();
            let memo_text_for_monitor = memo_text.clone();
            let memo_alarm_editor_for_monitor = memo_alarm_editor.clone();
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
                                    memo_categories_for_monitor.set_text("");
                                    memo_meta_for_monitor.set_label("Keine Notiz ausgewählt");
                                    memo_text_for_monitor
                                        .buffer()
                                        .set_text("Noch keine Notizdaten verfügbar.");
                                    memo_alarm_editor_for_monitor.set_component(None, "");
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
                                        memo_categories_for_monitor
                                            .set_text(&component_categories(&memo.component));
                                        memo_meta_for_monitor
                                            .set_label(&format!("VJOURNAL · {}", memo.updated));
                                        memo_text_for_monitor.buffer().set_text(&memo.description);
                                        memo_alarm_editor_for_monitor
                                            .set_component(Some(&memo.component), &memo.summary);
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
                    if let Some(page) = page_weak_for_monitor.upgrade() {
                        refresh_memo_category_model(&page, &memo_list_for_monitor);
                    }
                }
            });
        }

        let content_page = adw::NavigationPage::builder()
            .title("Notiz-Details")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        let split_for_navigation = split.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let is_back = key == gtk::gdk::Key::Escape
                || (key == gtk::gdk::Key::Left
                    && modifiers.contains(gtk::gdk::ModifierType::ALT_MASK));
            if is_back
                && split_for_navigation.is_collapsed()
                && split_for_navigation.shows_content()
            {
                split_for_navigation.set_show_content(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        split.add_controller(key_controller);
        split.set_parent(self);
    }

    fn open_memo_dialog(
        &self,
        memo_list: gtk::ListBox,
        memo_records: Rc<RefCell<Vec<MemoRecord>>>,
        empty_state: gtk::Label,
        memo_to_edit: Option<MemoRecord>,
    ) {
        let editing = memo_to_edit.is_some();
        let initial_component = memo_to_edit
            .as_ref()
            .map(|memo| memo.component.clone())
            .unwrap_or_else(|| {
                build_memo_component("Neue Notiz", "", "", &[])
                    .expect("the default memo component is valid")
            });
        let initial_draft = match MemoEditorDraft::parse(&initial_component) {
            Ok(draft) => draft,
            Err(error) => {
                tracing::warn!(%error, "Could not create memo editor draft");
                return;
            }
        };
        let draft_for_save = Rc::new(RefCell::new(initial_draft.clone()));
        let dirty = Rc::new(Cell::new(false));
        let operation_in_progress = Rc::new(Cell::new(false));
        let request_generation = Rc::new(Cell::new(0u64));
        let close_confirmed = Rc::new(Cell::new(false));
        let close_prompt_open = Rc::new(Cell::new(false));
        let attachment_loading = Rc::new(Cell::new(false));

        let dialog = adw::Window::builder()
            .title(if editing {
                "Notiz bearbeiten"
            } else {
                "Neue Notiz"
            })
            .modal(true)
            .default_width(680)
            .default_height(760)
            .build();
        if let Some(root) = self.root() {
            if let Ok(window) = root.downcast::<gtk::Window>() {
                dialog.set_transient_for(Some(&window));
            }
        }

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let cancel_button = gtk::Button::with_label("Abbrechen");
        header_bar.pack_start(&cancel_button);
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

        let source_model = gtk::StringList::new(&["Notizlisten werden geladen…"]);
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
        let title_entry = adw::EntryRow::builder().title("Summary").build();
        details.add(&title_entry);
        let dtstart_entry = adw::EntryRow::builder()
            .title("DTSTART (iCalendar-Wert)")
            .build();
        dtstart_entry.set_tooltip_text(Some("z. B. 20260822 oder 20260822T120000Z"));
        details.add(&dtstart_entry);
        let class_entry = adw::EntryRow::builder().title("Class").build();
        class_entry.set_tooltip_text(Some("z. B. PUBLIC, PRIVATE oder CONFIDENTIAL"));
        details.add(&class_entry);
        let status_entry = adw::EntryRow::builder().title("Status").build();
        status_entry.set_tooltip_text(Some("RFC5545-STATUS, z. B. DRAFT oder FINAL"));
        details.add(&status_entry);
        let url_entry = adw::EntryRow::builder().title("URL").build();
        details.add(&url_entry);
        let categories_entry = adw::EntryRow::builder()
            .title("Kategorien (Komma getrennt)")
            .build();
        categories_entry.set_tooltip_text(Some("z. B. Arbeit, Privat"));
        details.add(&categories_entry);
        let alarm_editor = AlarmEditor::new(Some(&initial_component), &initial_draft.summary);
        let alarm_dirty_gesture = gtk::GestureClick::new();
        let dirty_for_alarm = Rc::clone(&dirty);
        alarm_dirty_gesture.connect_pressed(move |_, _, _, _| dirty_for_alarm.set(true));
        alarm_editor.widget().add_controller(alarm_dirty_gesture);
        details.add(alarm_editor.widget());
        content_box.append(&details);

        title_entry.set_text(&initial_draft.summary);
        dtstart_entry.set_text(initial_draft.dtstart.as_deref().unwrap_or_default());
        class_entry.set_text(initial_draft.class.as_deref().unwrap_or_default());
        status_entry.set_text(initial_draft.status.as_deref().unwrap_or_default());
        url_entry.set_text(initial_draft.url.as_deref().unwrap_or_default());
        categories_entry.set_text(&serialize_categories_vec(&initial_draft.categories));

        let description_group = adw::PreferencesGroup::builder()
            .title("Beschreibung")
            .build();
        let description_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(180)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        description_view
            .buffer()
            .set_text(&initial_draft.description);
        description_group.add(&description_view);
        content_box.append(&description_group);

        let attachment_group = adw::PreferencesGroup::builder().title("Anhänge").build();
        let attachment_buffer = gtk::TextBuffer::new(None);
        attachment_buffer.set_text(&memo_attachments_editor_text(&initial_draft.attachments));
        let attachment_view = gtk::TextView::builder()
            .buffer(&attachment_buffer)
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(120)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        attachment_view.set_monospace(true);
        attachment_view.set_tooltip_text(Some(
            "Eine URI oder INLINE:<Inhalt> pro Zeile; Inhalte werden nicht ausgeführt",
        ));
        let attachment_picker = gtk::Button::with_label("URI/Datei hinzufügen");
        let attachment_controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(8)
            .build();
        let attachment_hint = gtk::Label::builder()
            .label("URI:… oder INLINE:… (eine Zeile pro Anhang)")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(vec!["dim-label"])
            .build();
        attachment_controls.append(&attachment_hint);
        attachment_controls.append(&attachment_picker);
        let attachment_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        attachment_box.append(&attachment_view);
        attachment_box.append(&attachment_controls);
        attachment_group.add(&attachment_box);
        content_box.append(&attachment_group);

        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();
        toolbar_view.set_content(Some(&scroll));
        dialog.set_content(Some(&toolbar_view));

        for entry in [
            title_entry.clone(),
            dtstart_entry.clone(),
            class_entry.clone(),
            status_entry.clone(),
            url_entry.clone(),
            categories_entry.clone(),
        ] {
            let dirty_for_entry = Rc::clone(&dirty);
            entry.connect_changed(move |_| dirty_for_entry.set(true));
        }
        let dirty_for_description = Rc::clone(&dirty);
        description_view
            .buffer()
            .connect_changed(move |_| dirty_for_description.set(true));
        let dirty_for_attachments = Rc::clone(&dirty);
        attachment_buffer.connect_changed(move |_| dirty_for_attachments.set(true));

        let attachment_loading_for_picker = Rc::clone(&attachment_loading);
        let attachment_buffer_for_picker = attachment_buffer.clone();
        attachment_picker.connect_clicked(move |button| {
            let Some(root) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            attachment_loading_for_picker.set(true);
            let file_dialog = gtk::FileDialog::builder()
                .title("Memo-Anhänge auswählen")
                .modal(true)
                .build();
            let buffer_for_result = attachment_buffer_for_picker.clone();
            let loading_for_result = Rc::clone(&attachment_loading_for_picker);
            file_dialog.open_multiple(Some(&root), None::<&gio::Cancellable>, move |result| {
                if let Ok(files) = result {
                    let mut values = buffer_for_result
                        .text(
                            &buffer_for_result.start_iter(),
                            &buffer_for_result.end_iter(),
                            false,
                        )
                        .lines()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                    for index in 0..files.n_items() {
                        let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                            continue;
                        };
                        let uri = file.uri().to_string();
                        if !uri.is_empty() {
                            values.push(format!("URI:{uri}"));
                        }
                    }
                    buffer_for_result.set_text(&values.join("\n"));
                }
                loading_for_result.set(false);
            });
        });

        let dialog_weak_for_close = dialog.downgrade();
        let close_confirmed_for_close = Rc::clone(&close_confirmed);
        let close_prompt_open_for_close = Rc::clone(&close_prompt_open);
        let dirty_for_close = Rc::clone(&dirty);
        let operation_for_close = Rc::clone(&operation_in_progress);
        let request_generation_for_close = Rc::clone(&request_generation);
        let save_button_for_close = save_button.clone();
        dialog.connect_close_request(move |_| {
            if close_confirmed_for_close.replace(false) {
                return glib::Propagation::Proceed;
            }
            match memo_close_decision(
                dirty_for_close.get(),
                operation_for_close.get(),
                close_prompt_open_for_close.get(),
            ) {
                MemoCloseDecision::Allow => return glib::Propagation::Proceed,
                MemoCloseDecision::BlockSave => {
                    if let Some(dialog) = dialog_weak_for_close.upgrade() {
                        show_toast_for_widget(&dialog, "Speichern läuft noch; bitte kurz warten");
                    }
                    return glib::Propagation::Stop;
                }
                MemoCloseDecision::KeepPrompt => return glib::Propagation::Stop,
                MemoCloseDecision::Prompt => close_prompt_open_for_close.set(true),
            }
            let confirmation = adw::AlertDialog::new(
                Some("Ungespeicherte Memo-Änderungen"),
                Some("Die Notiz wurde seit dem letzten bestätigten Speichern geändert."),
            );
            confirmation.add_responses(&[
                ("cancel", "Abbrechen"),
                ("save", "Speichern"),
                ("discard", "Verwerfen"),
            ]);
            confirmation.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            let dialog_weak_for_response = dialog_weak_for_close.clone();
            let close_confirmed_for_response = Rc::clone(&close_confirmed_for_close);
            let close_prompt_open_for_response = Rc::clone(&close_prompt_open_for_close);
            let request_generation_for_response = Rc::clone(&request_generation_for_close);
            let save_button_for_response = save_button_for_close.clone();
            confirmation.connect_response(None, move |alert, response| {
                alert.close();
                close_prompt_open_for_response.set(false);
                let Some(dialog) = dialog_weak_for_response.upgrade() else {
                    return;
                };
                match response {
                    "discard" => {
                        request_generation_for_response
                            .set(request_generation_for_response.get().wrapping_add(1));
                        close_confirmed_for_response.set(true);
                        dialog.close();
                    }
                    "save" => save_button_for_response.emit_clicked(),
                    _ => {}
                }
            });
            if let Some(dialog) = dialog_weak_for_close.upgrade() {
                confirmation.present(Some(&dialog));
            } else {
                close_prompt_open_for_close.set(false);
            }
            glib::Propagation::Stop
        });
        let dialog_weak_for_cancel = dialog.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak_for_cancel.upgrade() {
                dialog.close();
            }
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_row_for_load = source_row.clone();
        let save_button_for_load = save_button.clone();
        let local_calendar_available = CalendarStoreService::new().is_writable();
        let edit_source_uid = memo_to_edit
            .as_ref()
            .map(|memo| memo.source_uid.clone())
            .filter(|uid| !uid.trim().is_empty());
        let preferred_source_uid = AppSettings::try_new()
            .map(|settings| settings.default_memo_source_uid())
            .filter(|uid| !uid.trim().is_empty());
        save_button.set_sensitive(edit_source_uid.is_some());
        let edit_source_uid_for_load = edit_source_uid.clone();
        let preferred_source_uid_for_load = preferred_source_uid.clone();
        glib::spawn_future_local(async move {
            let discovery =
                eds_service::discover_sources(eds_service::EdsSourceKind::Calendar).await;
            if let Some(error) = discovery.error.as_deref() {
                tracing::warn!(%error, "Could not reach EDS memo lists for memo dialog");
            }
            let active_sources: Vec<_> = discovery
                .sources
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
            let preserved_source_uid = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .filter(|uid| {
                    *uid != "local-calendar" && !uids.iter().any(|candidate| candidate == uid)
                });
            if let Some(uid) = preserved_source_uid {
                names.push(format!("Nicht verfügbare Quelle · {uid}"));
                uids.push(uid.to_string());
            }
            if local_calendar_available {
                names.push("Lokale Notizliste".to_string());
                uids.push("local-calendar".to_string());
            }
            if names.is_empty() {
                names.push(if discovery.error.is_some() {
                    "Notizlisten sind nicht erreichbar".to_string()
                } else {
                    "Keine aktive Notizliste verfügbar".to_string()
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            let selected_index = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
                .or_else(|| (!uids.is_empty()).then_some(0));
            drop(uids);
            if let Some(index) = selected_index {
                source_row_for_load.set_selected(index as u32);
                save_button_for_load.set_sensitive(true);
            } else {
                save_button_for_load.set_sensitive(false);
            }
        });

        let dialog_weak = dialog.downgrade();
        let page_weak = self.downgrade();
        let source_uids_for_save = Rc::clone(&source_uids);
        let draft_for_save_signal = Rc::clone(&draft_for_save);
        let operation_for_save = Rc::clone(&operation_in_progress);
        let request_generation_for_save = Rc::clone(&request_generation);
        let close_confirmed_for_save = Rc::clone(&close_confirmed);
        let attachment_loading_for_save = Rc::clone(&attachment_loading);
        let memo_to_edit_for_save = memo_to_edit.clone();
        let source_display_name_for_save = memo_to_edit
            .as_ref()
            .map(|memo| memo.source_display_name.clone());
        let save_button_for_signal = save_button.clone();
        save_button_for_signal.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            if operation_for_save.get() {
                return;
            }
            let selected = source_row.selected() as usize;
            let source_uid = if let Some(memo) = memo_to_edit_for_save.as_ref() {
                memo.source_uid.clone()
            } else if let Some(source_uid) = source_uids_for_save.borrow().get(selected).cloned() {
                source_uid
            } else {
                show_toast_for_widget(&dialog, "Keine aktive Notizliste ist ausgewählt");
                return;
            };
            if source_uid.trim().is_empty() {
                show_toast_for_widget(&dialog, "Die ausgewählte Notizliste ist ungültig");
                return;
            }
            if let Some(error) = memo_attachment_save_error(attachment_loading_for_save.get()) {
                show_toast_for_widget(&dialog, error);
                return;
            }
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let attachment_text = attachment_buffer.text(
                &attachment_buffer.start_iter(),
                &attachment_buffer.end_iter(),
                false,
            );
            let mut draft = draft_for_save_signal.borrow().clone();
            draft.summary = title_entry.text().to_string();
            draft.dtstart = optional_text(&dtstart_entry.text());
            draft.class = optional_text(&class_entry.text());
            draft.status = optional_text(&status_entry.text());
            draft.url = optional_text(&url_entry.text());
            draft.categories = parse_categories_input(&categories_entry.text());
            draft.description = description.to_string();
            draft.attachments =
                match parse_memo_attachment_editor_text(&attachment_text, &draft.attachments) {
                    Ok(attachments) => attachments,
                    Err(error) => {
                        show_toast_for_widget(&dialog, &error);
                        return;
                    }
                };
            alarm_editor.set_summary(&draft.summary);
            draft.alarms = alarm_editor.alarms();
            let mut component = match draft.apply_to_component() {
                Ok(component) => component,
                Err(error) => {
                    show_toast_for_widget(&dialog, &error);
                    return;
                }
            };
            let modified = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            component = crate::services::calendar_store_service::update_ical_property(
                &component,
                "LAST-MODIFIED",
                Some(&modified),
            );
            let operation_uid = memo_to_edit_for_save.as_ref().map(|memo| memo.uid.clone());
            let expected_uid = operation_uid.clone();
            let generation = request_generation_for_save.get().wrapping_add(1);
            request_generation_for_save.set(generation);
            operation_for_save.set(true);
            save_button.set_sensitive(false);
            let source_uid_for_model = source_uid.clone();
            let source_uid_for_selection = source_uid_for_model.clone();
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let service = ECalService::new();
                let result = match operation_uid.as_deref() {
                    Some(uid) => service.modify_component(
                        &source_uid,
                        ECalComponentType::Memo,
                        uid,
                        &component,
                    ),
                    None => {
                        service.create_component(&source_uid, ECalComponentType::Memo, &component)
                    }
                };
                if let Err(error) = sender.send_blocking(result) {
                    tracing::debug!(%error, "Memo mutation result was dropped");
                }
            });

            let dialog_weak_for_result = dialog_weak.clone();
            let page_weak_for_result = page_weak.clone();
            let memo_list_for_result = memo_list.clone();
            let memo_records_for_result = Rc::clone(&memo_records);
            let empty_state_for_result = empty_state.clone();
            let save_button_for_result = save_button.clone();
            let operation_for_result = Rc::clone(&operation_for_save);
            let request_generation_for_result = Rc::clone(&request_generation_for_save);
            let close_confirmed_for_result = Rc::clone(&close_confirmed_for_save);
            let draft_for_result = Rc::clone(&draft_for_save_signal);
            let source_display_name_for_result = source_display_name_for_save.clone();
            glib::spawn_future_local(async move {
                let result = receiver.recv().await;
                let Some(dialog) = dialog_weak_for_result.upgrade() else {
                    return;
                };
                if !memo_result_is_current(
                    request_generation_for_result.get(),
                    generation,
                    dialog.is_visible(),
                ) {
                    return;
                }
                operation_for_result.set(false);
                save_button_for_result.set_sensitive(true);
                match result {
                    Ok(Ok(result)) => {
                        let (component, confirmed_draft) = match
                            confirmed_memo_component(result.component)
                        {
                            Ok(confirmed) => confirmed,
                            Err(error) => {
                                show_toast_for_widget(
                                    &dialog,
                                    &format!("Bestätigte Memo-Komponente fehlt oder ist ungültig: {error}"),
                                );
                                return;
                            }
                        };
                        let uid = match confirmed_memo_uid(
                            expected_uid.as_deref(),
                            &result.uid,
                            &confirmed_draft.uid,
                        ) {
                            Ok(uid) => uid,
                            Err(error) => {
                                show_toast_for_widget(
                                    &dialog,
                                    &format!("Bestätigte Memo-Identität ist ungültig: {error}"),
                                );
                                return;
                            }
                        };
                        let source_display_name =
                            source_display_name_for_result.unwrap_or_else(|| {
                                if source_uid_for_model == "local-calendar" {
                                    "Lokale Notizliste".to_string()
                                } else {
                                    "synchronisiertem Notizbestand".to_string()
                                }
                            });
                        let memo = MemoRecord {
                            uid: uid.clone(),
                            source_uid: source_uid_for_model,
                            source_display_name,
                            component,
                            summary: confirmed_draft.summary.clone(),
                            description: confirmed_draft.description.clone(),
                            updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                        };
                        *draft_for_result.borrow_mut() = confirmed_draft;
                        upsert_memo_row(
                            &memo_list_for_result,
                            Rc::clone(&memo_records_for_result),
                            memo,
                        );
                        select_memo_row(&memo_list_for_result, &source_uid_for_selection, &uid);
                        empty_state_for_result.set_visible(false);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            refresh_memo_category_model(&page, &memo_list_for_result);
                            let message = if editing {
                                format!("Notiz aktualisiert ({uid})")
                            } else {
                                format!("Notiz gespeichert ({uid})")
                            };
                            page.show_toast(&message);
                        }
                        close_confirmed_for_result.set(true);
                        dialog.close();
                    }
                    Ok(Err(error)) => show_toast_for_widget(
                        &dialog,
                        &format!("Notiz konnte nicht gespeichert werden: {error}"),
                    ),
                    Err(error) => show_toast_for_widget(
                        &dialog,
                        &format!("Notiz-Speicherergebnis fehlt: {error}"),
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

fn select_memo_row(memo_list: &gtk::ListBox, source_uid: &str, uid: &str) -> bool {
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let matches = unsafe {
            row.data::<String>("memo-id")
                .is_some_and(|value| value.as_ref() == uid)
                && row
                    .data::<String>("memo-source-uid")
                    .is_some_and(|value| value.as_ref() == source_uid)
        };
        if matches {
            memo_list.select_row(Some(&row));
            return true;
        }
    }
    false
}

fn append_memo_row(memo_list: &gtk::ListBox, memo: &MemoRecord) {
    let row = gtk::ListBoxRow::new();
    unsafe {
        row.set_data("memo-id", memo.uid.clone());
        row.set_data("memo-source-uid", memo.source_uid.clone());
        row.set_data("memo-search", memo_search_text(memo));
        row.set_data("memo-categories", component_categories(&memo.component));
        row.set_data("memo-sort-updated", memo.updated.to_lowercase());
        row.set_data("memo-sort-title", memo.summary.to_lowercase());
        row.set_data("memo-sort-source", memo.source_display_name.to_lowercase());
    }
    let title = glib::markup_escape_text(&memo.summary);
    let subtitle = glib::markup_escape_text(&memo.updated);
    let action_row = adw::ActionRow::builder()
        .title(title.as_str())
        .subtitle(subtitle.as_str())
        .use_markup(true)
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

fn refresh_memo_category_model(page: &MemosPage, memo_list: &gtk::ListBox) {
    let Some(model) = page.imp().memo_category_model.borrow().as_ref().cloned() else {
        return;
    };
    let Some(selector) = page.imp().memo_category_selector.borrow().as_ref().cloned() else {
        return;
    };

    let mut categories = CalendarCategoryStore::default()
        .load()
        .into_iter()
        .map(|category| category.name)
        .collect::<Vec<_>>();
    let mut child = memo_list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
            continue;
        };
        let values = unsafe {
            row.data::<String>("memo-categories")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default()
        };
        categories.extend(
            values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    categories.sort_by_cached_key(|category| category.to_lowercase());
    categories.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

    let current_filter = page.imp().memo_category_filter.borrow().clone();
    let selected = current_filter
        .as_deref()
        .and_then(|filter| {
            categories
                .iter()
                .position(|category| category.eq_ignore_ascii_case(filter))
        })
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    if current_filter.is_some() && selected == 0 {
        page.imp().memo_category_filter.replace(None);
    }

    let mut names = vec!["Alle Kategorien".to_string()];
    names.extend(categories);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &name_refs);
    if selector.selected() != selected {
        selector.set_selected(selected);
    }
}

fn memo_matches_filter(
    memo: &MemoRecord,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &str,
) -> bool {
    source_filter.is_none_or(|source_uid| memo.source_uid == source_uid)
        && category_filter.is_none_or(|category| {
            component_categories(&memo.component)
                .split(',')
                .map(str::trim)
                .any(|value| value.eq_ignore_ascii_case(category))
        })
        && (query.is_empty() || memo_search_text(memo).contains(query))
}

fn apply_memo_list_filter(
    memo_list: &gtk::ListBox,
    source_filter: Option<String>,
    category_filter: Option<String>,
    query: String,
) {
    memo_list.set_filter_func(move |row| unsafe {
        let source_matches = source_filter.as_deref().is_none_or(|source_uid| {
            row.data::<String>("memo-source-uid")
                .is_some_and(|value| value.as_ref() == source_uid)
        });
        let category_matches = category_filter.as_deref().is_none_or(|category| {
            row.data::<String>("memo-categories").is_some_and(|value| {
                value
                    .as_ref()
                    .split(',')
                    .map(str::trim)
                    .any(|value| value.eq_ignore_ascii_case(category))
            })
        });
        let query_matches = query.is_empty()
            || row
                .data::<String>("memo-search")
                .is_some_and(|value| value.as_ref().contains(&query));
        source_matches && category_matches && query_matches
    });
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoCloseDecision {
    Allow,
    Prompt,
    KeepPrompt,
    BlockSave,
}

fn memo_close_decision(dirty: bool, saving: bool, prompt_open: bool) -> MemoCloseDecision {
    if saving {
        MemoCloseDecision::BlockSave
    } else if !dirty {
        MemoCloseDecision::Allow
    } else if prompt_open {
        MemoCloseDecision::KeepPrompt
    } else {
        MemoCloseDecision::Prompt
    }
}

fn memo_result_is_current(
    current_generation: u64,
    expected_generation: u64,
    dialog_visible: bool,
) -> bool {
    current_generation == expected_generation && dialog_visible
}

fn confirmed_memo_component(
    component: Option<String>,
) -> Result<(String, MemoEditorDraft), String> {
    let component = component
        .filter(|component| !component.trim().is_empty())
        .ok_or_else(|| "ECal lieferte keine vollständige Komponente".to_string())?;
    let draft = MemoEditorDraft::from_component(&component).map_err(|error| format!("{error}"))?;
    Ok((component, draft))
}

fn confirmed_memo_uid(
    expected_uid: Option<&str>,
    result_uid: &str,
    component_uid: &str,
) -> Result<String, String> {
    if let Some(expected_uid) = expected_uid {
        if component_uid != expected_uid {
            return Err("Komponenten-UID stimmt nicht mit der Anfrage überein".to_string());
        }
        return Ok(expected_uid.to_string());
    }
    if !result_uid.trim().is_empty() && result_uid != component_uid {
        return Err("ECal-Ergebnis und bestätigte Komponente haben verschiedene UIDs".to_string());
    }
    let uid = if result_uid.trim().is_empty() {
        component_uid
    } else {
        result_uid
    };
    (!uid.trim().is_empty())
        .then(|| uid.to_string())
        .ok_or_else(|| "Die bestätigte Memo-Komponente hat keine UID".to_string())
}

#[cfg(test)]
fn memo_delete_confirmed(response: &str) -> bool {
    response == "confirm"
}

fn memo_attachment_save_error(loading: bool) -> Option<&'static str> {
    loading.then_some("Anhänge werden noch geladen; bitte warten")
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn serialize_categories_vec(categories: &[String]) -> String {
    categories
        .iter()
        .map(|category| escape_ical_text(category.trim()))
        .filter(|category| !category.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_categories_input(value: &str) -> Vec<String> {
    let mut categories = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            if !current.trim().is_empty() {
                categories.push(unescape_ical_text(current.trim()));
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.trim().is_empty() {
        categories.push(unescape_ical_text(current.trim()));
    }
    categories
}

fn memo_attachment_editor_line(attachment: &MemoAttachment) -> String {
    if attachment.is_uri() && !attachment.is_inline() {
        let uri = attachment.uri.as_deref().unwrap_or_default();
        format!("URI:{uri}")
    } else {
        format!(
            "INLINE:{}",
            attachment.inline_content.as_deref().unwrap_or_default()
        )
    }
}

fn memo_attachments_editor_text(attachments: &[MemoAttachment]) -> String {
    attachments
        .iter()
        .map(memo_attachment_editor_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn attachment_with_editor_value(original: &MemoAttachment, line: &str) -> Option<MemoAttachment> {
    let mut attachment = original.clone();
    if let Some(uri) = line.strip_prefix("URI:") {
        let uri = uri.trim();
        if uri.is_empty() {
            return None;
        }
        attachment.uri = Some(uri.to_string());
        attachment.inline_content = None;
    } else if let Some(content) = line.strip_prefix("INLINE:") {
        attachment.uri = None;
        attachment.inline_content = Some(content.to_string());
    } else {
        let uri = line.trim();
        if uri.is_empty() {
            return None;
        }
        attachment.uri = Some(uri.to_string());
        attachment.inline_content = None;
    }
    Some(attachment)
}

fn parse_memo_attachment_editor_text(
    value: &str,
    existing: &[MemoAttachment],
) -> Result<Vec<MemoAttachment>, String> {
    let mut used_existing = vec![false; existing.len()];
    let mut attachments = Vec::new();
    for (line_index, raw_line) in value.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains('\r') || line.contains('\n') {
            return Err("Die Anhangsliste enthält ungültige Zeilenumbrüche".to_string());
        }
        if let Some((index, attachment)) =
            existing.iter().enumerate().find(|(index, attachment)| {
                !used_existing[*index] && memo_attachment_editor_line(attachment) == line
            })
        {
            used_existing[index] = true;
            attachments.push(attachment.clone());
            continue;
        }
        if let Some((index, attachment)) = existing
            .get(line_index)
            .filter(|_| !used_existing.get(line_index).copied().unwrap_or(true))
            .map(|attachment| (line_index, attachment))
        {
            if let Some(attachment) = attachment_with_editor_value(attachment, line) {
                used_existing[index] = true;
                attachments.push(attachment);
                continue;
            }
        }
        if let Some(uri) = line.strip_prefix("URI:") {
            let uri = uri.trim();
            if uri.is_empty() {
                return Err("Ein URI-Anhang darf nicht leer sein".to_string());
            }
            attachments.push(MemoAttachment::uri(uri.to_string()));
        } else if let Some(content) = line.strip_prefix("INLINE:") {
            attachments.push(MemoAttachment::inline(content.to_string()));
        } else {
            // A bare line is accepted as a URI for the same compact workflow
            // as Evolution's attachment page.  It is only serialized; it is
            // never opened or fetched by this editor.
            attachments.push(MemoAttachment::uri(line.to_string()));
        }
    }
    Ok(attachments)
}

fn build_memo_component(
    title: &str,
    description: &str,
    categories: &str,
    alarms: &[Alarm],
) -> Result<String, String> {
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
    let categories = serialize_categories(categories);
    if !categories.is_empty() {
        component.push(format!("CATEGORIES:{categories}"));
    }
    component.push("END:VJOURNAL".to_string());
    Ok(replace_component_alarms(
        &format!("{}\r\n", component.join("\r\n")),
        alarms,
    ))
}

fn escape_ical_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

fn serialize_categories(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .map(escape_ical_text)
        .collect::<Vec<_>>()
        .join(",")
}

fn component_categories(component: &str) -> String {
    component
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .find_map(|line| {
            let (property, value) = line.split_once(':')?;
            let name = property.split(';').next().unwrap_or(property).trim();
            (name.eq_ignore_ascii_case("CATEGORIES")).then(|| unescape_ical_text(value))
        })
        .unwrap_or_default()
}

fn unescape_ical_text(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::calendar_store_service::MemoAttachmentParameter;

    #[test]
    fn memo_component_serialization_is_ical_safe() {
        let component = build_memo_component(
            "A, note; title",
            "Line one\nLine two",
            "Work, Personal",
            &[],
        )
        .expect("memo component should be valid");
        assert!(component.starts_with("BEGIN:VJOURNAL\r\n"));
        assert!(component.contains("SUMMARY:A\\, note\\; title\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.contains("CATEGORIES:Work,Personal\r\n"));
        assert!(component.ends_with("END:VJOURNAL\r\n"));
    }

    #[test]
    fn memo_component_rejects_empty_title() {
        assert!(build_memo_component("  ", "body", "", &[]).is_err());
    }

    #[test]
    fn memo_categories_roundtrip_through_component_text() {
        let component = "BEGIN:VJOURNAL\r\nCATEGORIES:Work\\, urgent,Personal\r\nEND:VJOURNAL\r\n";
        assert_eq!(component_categories(component), "Work, urgent,Personal");
        assert_eq!(serialize_categories("Work, Personal"), "Work,Personal");
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
        assert!(memo_matches_filter(&memo, None, Some("project-x"), ""));
        assert!(!memo_matches_filter(&memo, None, Some("personal"), ""));
    }

    #[test]
    fn memo_attachment_editor_keeps_existing_uri_and_inline_values_in_order() {
        let mut first = MemoAttachment::uri("https://example.test/a");
        first.filename = Some("a.txt".to_string());
        first.fmttype = Some("text/plain".to_string());
        first.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-KEEP".to_string(),
            value: Some("yes".to_string()),
        });
        let second = MemoAttachment::inline("UE5H");
        let existing = vec![first.clone(), second.clone()];
        let text = memo_attachments_editor_text(&existing);
        let parsed = parse_memo_attachment_editor_text(&text, &existing)
            .expect("unchanged attachment editor text should parse");
        assert_eq!(parsed, existing);

        let changed = parse_memo_attachment_editor_text(
            "URI:https://example.test/a\nINLINE:UE5H\nURI:https://example.test/c",
            &existing,
        )
        .expect("new URI should be accepted");
        assert_eq!(changed.len(), 3);
        assert_eq!(changed[0].filename.as_deref(), Some("a.txt"));
        assert_eq!(changed[1].inline_content.as_deref(), Some("UE5H"));
        assert_eq!(changed[2].uri.as_deref(), Some("https://example.test/c"));
    }

    #[test]
    fn memo_attachment_editor_changes_retain_existing_metadata() {
        let mut uri = MemoAttachment::uri("https://example.test/old");
        uri.filename = Some("memo.txt".to_string());
        uri.fmttype = Some("text/plain".to_string());
        uri.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-SAFE".to_string(),
            value: Some("keep".to_string()),
        });
        let mut inline = MemoAttachment::inline("old-payload");
        inline.filename = Some("payload.bin".to_string());
        inline.fmttype = Some("application/octet-stream".to_string());
        inline.unknown_parameters.push(MemoAttachmentParameter {
            name: "X-INLINE".to_string(),
            value: Some("keep".to_string()),
        });
        let existing = vec![uri, inline];
        let changed = parse_memo_attachment_editor_text(
            "URI:https://example.test/new\nINLINE:new-payload",
            &existing,
        )
        .expect("edited attachment values should parse");
        assert_eq!(changed[0].uri.as_deref(), Some("https://example.test/new"));
        assert_eq!(changed[0].filename.as_deref(), Some("memo.txt"));
        assert_eq!(changed[0].fmttype.as_deref(), Some("text/plain"));
        assert_eq!(changed[0].unknown_parameters[0].name, "X-SAFE");
        assert_eq!(changed[1].inline_content.as_deref(), Some("new-payload"));
        assert_eq!(changed[1].filename.as_deref(), Some("payload.bin"));
        assert_eq!(changed[1].unknown_parameters[0].name, "X-INLINE");
    }

    #[test]
    fn memo_dialog_close_and_mutation_guards_cover_cancel_dirty_and_stale_results() {
        assert_eq!(
            memo_close_decision(false, false, false),
            MemoCloseDecision::Allow
        );
        assert_eq!(
            memo_close_decision(true, false, false),
            MemoCloseDecision::Prompt
        );
        assert_eq!(
            memo_close_decision(true, false, true),
            MemoCloseDecision::KeepPrompt
        );
        assert_eq!(
            memo_close_decision(true, true, false),
            MemoCloseDecision::BlockSave
        );
        assert!(!memo_delete_confirmed("cancel"));
        assert!(memo_delete_confirmed("confirm"));
        assert!(memo_result_is_current(4, 4, true));
        assert!(!memo_result_is_current(4, 3, true));
        assert!(!memo_result_is_current(4, 4, false));
    }

    #[test]
    fn invalid_and_loading_attachment_states_are_rejected_before_component_apply() {
        let existing = Vec::new();
        assert!(parse_memo_attachment_editor_text("URI:", &existing).is_err());
        assert_eq!(
            memo_attachment_save_error(true),
            Some("Anhänge werden noch geladen; bitte warten")
        );
        assert_eq!(memo_attachment_save_error(false), None);
        let mut draft = MemoEditorDraft::from_component(
            "BEGIN:VJOURNAL\r\nUID:memo-invalid\r\nSUMMARY:Valid\r\nEND:VJOURNAL\r\n",
        )
        .expect("valid draft should parse");
        let mut invalid_attachment = MemoAttachment::uri("https://example.test/a\nunsafe");
        invalid_attachment.filename = Some("bad\nname".to_string());
        draft.attachments = vec![invalid_attachment];
        assert!(draft.apply().is_err());
    }

    #[test]
    fn confirmed_memo_result_is_required_before_model_data_can_be_built() {
        assert!(confirmed_memo_component(None).is_err());
        assert!(confirmed_memo_component(Some(String::new())).is_err());
        assert!(
            confirmed_memo_component(Some(
                "BEGIN:VEVENT\r\nUID:not-a-memo\r\nEND:VEVENT\r\n".to_string()
            ))
            .is_err()
        );
        let component = "BEGIN:VJOURNAL\r\nUID:confirmed\r\nSUMMARY:Confirmed\r\nEND:VJOURNAL\r\n";
        let (confirmed, draft) = confirmed_memo_component(Some(component.to_string()))
            .expect("complete confirmed memo should be accepted");
        assert_eq!(confirmed, component);
        assert_eq!(draft.uid, "confirmed");
        assert_eq!(draft.summary, "Confirmed");
        assert!(confirmed_memo_uid(Some("expected"), "expected", "other").is_err());
        assert!(confirmed_memo_uid(None, "one", "two").is_err());
        assert_eq!(
            confirmed_memo_uid(None, "", "component").expect("component UID is usable"),
            "component"
        );
    }
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
