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
        let search_toolbar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_bottom(4)
            .build();
        search_toolbar.append(&search_entry);
        search_toolbar.append(&sort_selector);
        search_toolbar.append(&direction_button);
        search_toolbar.append(&source_selector);
        search_toolbar.append(&category_selector);
        sidebar_box.append(&search_toolbar);

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

        let sidebar_outer_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        sidebar_outer_box.append(&sidebar_scroll);
        sidebar_outer_box.append(&crate::views::build_sidebar_module_switcher(
            "memos",
            &sidebar_outer_box,
        ));

        let sidebar_page = adw::NavigationPage::builder()
            .title("Notizen")
            .child(&sidebar_outer_box)
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

        let save_memo_btn_for_context = save_memo_btn.clone();
        let export_memo_btn_for_context = export_memo_btn.clone();
        let delete_memo_btn_for_context = delete_memo_btn.clone();
        let selected_memo_for_save = Rc::clone(&selected_memo);
        let memo_records_for_save = Rc::clone(&memo_records);
        let page_weak_for_save = self.downgrade();
        let memo_title_for_save = memo_title.clone();
        let memo_categories_for_save = memo_categories.clone();
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
            let categories = serialize_categories(&memo_categories_for_save.text());
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
                "CATEGORIES",
                (!categories.is_empty()).then_some(categories.as_str()),
            );
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
            let memo_categories_for_result = memo_categories_for_save.clone();
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
                        select_memo_row(&memo_list_for_result, &updated.source_uid, &updated.uid);
                        *selected_memo_for_result.borrow_mut() = Some(updated);
                        memo_title_for_result.set_text(&title);
                        memo_categories_for_result.set_text(&categories);
                        memo_text_for_result.buffer().set_text(&description);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            refresh_memo_category_model(&page, &memo_list_for_result);
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
        let memo_categories_for_delete = memo_categories.clone();
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
                "Die Notiz wird dauerhaft aus der synchronisierten Notizliste entfernt."
            };
            let memo_for_action = memo.clone();
            let selected_memo_for_action = Rc::clone(&selected_memo_for_delete);
            let memo_records_for_action = Rc::clone(&memo_records_for_delete);
            let page_weak_for_action = page_weak_for_delete.clone();
            let memo_list_for_action = memo_list_for_delete.clone();
            let empty_state_for_action = empty_state_for_delete.clone();
            let memo_title_for_action = memo_title_for_delete.clone();
            let memo_categories_for_action = memo_categories_for_delete.clone();
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
                    let memo_categories_for_result = memo_categories_for_action.clone();
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
        let split_for_selection = split.clone();
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
            move |_, row| {
                let Some(row) = row else {
                    *selected_memo_for_selection.borrow_mut() = None;
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
        split.set_min_sidebar_width(260.0);
        split.set_max_sidebar_width(360.0);
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
        let title_entry = adw::EntryRow::builder().title("Titel").build();
        details.add(&title_entry);
        let categories_entry = adw::EntryRow::builder()
            .title("Kategorien (Komma getrennt)")
            .build();
        categories_entry.set_tooltip_text(Some("z. B. Arbeit, Privat"));
        details.add(&categories_entry);
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
            let preserved_source_uid = preferred_source_uid.as_deref().filter(|uid| {
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
            let selected_index = preferred_source_uid
                .as_deref()
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid));
            drop(uids);
            if let Some(index) = selected_index {
                source_row_for_load.set_selected(index as u32);
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
                show_toast_for_widget(&dialog, "Keine aktive Notizliste ist ausgewählt");
                return;
            };
            let description = description_view.buffer().text(
                &description_view.buffer().start_iter(),
                &description_view.buffer().end_iter(),
                false,
            );
            let component = match build_memo_component(
                &title_entry.text(),
                &description,
                &categories_entry.text(),
            ) {
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
            let source_uid_for_selection = source_uid_for_model.clone();
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
                            "synchronisiertem Notizbestand".to_string()
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
                        select_memo_row(&memo_list_for_result, &source_uid_for_selection, &uid);
                        empty_state_for_result.set_visible(false);
                        if let Some(page) = page_weak_for_result.upgrade() {
                            refresh_memo_category_model(&page, &memo_list_for_result);
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

fn build_memo_component(
    title: &str,
    description: &str,
    categories: &str,
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

    #[test]
    fn memo_component_serialization_is_ical_safe() {
        let component =
            build_memo_component("A, note; title", "Line one\nLine two", "Work, Personal")
                .expect("memo component should be valid");
        assert!(component.starts_with("BEGIN:VJOURNAL\r\n"));
        assert!(component.contains("SUMMARY:A\\, note\\; title\r\n"));
        assert!(component.contains("DESCRIPTION:Line one\\nLine two\r\n"));
        assert!(component.contains("CATEGORIES:Work,Personal\r\n"));
        assert!(component.ends_with("END:VJOURNAL\r\n"));
    }

    #[test]
    fn memo_component_rejects_empty_title() {
        assert!(build_memo_component("  ", "body", "").is_err());
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
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
