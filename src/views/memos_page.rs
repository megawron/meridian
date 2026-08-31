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
use std::collections::HashSet;
use std::rc::Rc;

use crate::dialogs::event_dialog::AlarmEditor;
use crate::services::calendar_category_service::CalendarCategoryStore;
use crate::services::calendar_store_service::{
    CalendarStoreService, MemoAttachment, MemoEditorDraft, MemoRecord,
};
use crate::services::ecal_service::{
    CalendarDataOrigin, ECalChangeKind, ECalComponentType, ECalRecord, ECalService,
    ECalSourceCapability, ECalSyncEvent, load_snapshot_prefer_eds, parse_change_record,
};
use crate::services::memo_filter_service::{MemoCategoryFilter, MemoFilter, MemoSearchMode};
use crate::services::memo_surface_service::{
    MemoActionState, MemoIdentity, MemoPasteItem, MemoPreviewOrientation, MemoPreviewState,
    MemoSelection, build_clipboard_payload, build_forward_publish_payload, build_multi_export,
    build_print_document, plan_delete, plan_paste_into, preview_for_selection,
    validated_url_for_record,
};
use crate::services::reminder_service::{Alarm, replace_component_alarms};
use crate::services::settings_service::{AppSettings, normalize_memo_source_filter_value};
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
        pub memo_available_sources: RefCell<HashSet<String>>,
        pub memo_unavailable_sources: RefCell<HashSet<String>>,
        pub memo_writable_sources: RefCell<HashSet<String>>,
        pub memo_search_mode: Cell<MemoSearchMode>,
        pub memo_search_query: RefCell<String>,
        pub memo_selection: RefCell<MemoSelection>,
        pub memo_refresh_generation: Cell<u64>,
        pub memo_preview_state: RefCell<MemoPreviewState>,
        pub memo_list: RefCell<Option<gtk::ListBox>>,
        pub memo_records: RefCell<Option<Rc<RefCell<Vec<MemoRecord>>>>>,
        pub memo_empty_state: RefCell<Option<gtk::Label>>,
        pub new_memo_button: RefCell<Option<gtk::Button>>,
        pub search_entry: RefCell<Option<gtk::SearchEntry>>,
        pub memo_preview_label: RefCell<Option<gtk::Label>>,
        pub memo_preview_box: RefCell<Option<gtk::Box>>,
        pub memo_surface_buttons: RefCell<Option<Vec<gtk::Button>>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoSourceProjection {
    uid: String,
    display_name: String,
    connected: bool,
    available: bool,
    writable: bool,
}

fn project_memo_source_capabilities(
    capabilities: Vec<ECalSourceCapability>,
) -> Vec<MemoSourceProjection> {
    let mut seen = HashSet::new();
    let mut sources = capabilities
        .into_iter()
        .filter_map(|source| {
            let uid = source.source_uid.trim().to_string();
            (!uid.is_empty() && seen.insert(uid.clone())).then(|| MemoSourceProjection {
                uid,
                display_name: source.display_name,
                connected: source.connected,
                available: source.available && source.connected,
                writable: source.available && source.connected && source.writable,
            })
        })
        .collect::<Vec<_>>();
    sources.sort_by_cached_key(|source| (source.display_name.to_lowercase(), source.uid.clone()));
    sources
}

fn memo_source_projection_label(source: &MemoSourceProjection) -> String {
    let name = (!source.display_name.trim().is_empty())
        .then_some(source.display_name.as_str())
        .unwrap_or(source.uid.as_str());
    if !source.connected {
        format!("Nicht erreichbar · {name}")
    } else if !source.available {
        format!("Nicht verfügbar · {name}")
    } else if !source.writable {
        format!("Schreibgeschützt · {name}")
    } else {
        name.to_string()
    }
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

        let selection = self.imp().memo_selection.borrow().clone();
        while let Some(child) = memo_list.first_child() {
            memo_list.remove(&child);
        }
        for memo in memo_records.borrow().iter() {
            append_memo_row(&memo_list, memo);
        }

        refresh_memo_category_model(self, &memo_list);
        let generation = self.imp().memo_refresh_generation.get().wrapping_add(1);
        self.imp().memo_refresh_generation.set(generation);
        *self.imp().memo_selection.borrow_mut() = selection;
        self.imp()
            .memo_selection
            .borrow_mut()
            .restore_after_refresh(&memo_records.borrow(), generation);
        let restored_selection = self.imp().memo_selection.borrow().clone();
        restore_memo_selection_rows(&memo_list, &restored_selection);
        refresh_memo_filter_view(self, &memo_list, &memo_records, &empty_state);
    }

    fn build_ui(&self) {
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(240.0)
            .max_sidebar_width(240.0)
            .sidebar_width_fraction(0.0)
            .build();

        let settings = AppSettings::try_new();
        let initial_memo_search_mode = settings
            .as_ref()
            .map(|settings| MemoSearchMode::from_setting(&settings.memo_search_mode()))
            .unwrap_or_default();
        let initial_memo_category_filter = settings
            .as_ref()
            .map(|settings| settings.memo_category_filter())
            .unwrap_or_else(|| "any".to_string());
        let initial_memo_preview_state = MemoPreviewState {
            visible: settings
                .as_ref()
                .map(AppSettings::memo_preview_visible)
                .unwrap_or(true),
            orientation: settings
                .as_ref()
                .map(|settings| {
                    MemoPreviewOrientation::from_setting(&settings.memo_preview_orientation())
                })
                .unwrap_or_default(),
        };
        self.imp().memo_search_mode.set(initial_memo_search_mode);
        self.imp()
            .memo_category_filter
            .replace(category_filter_option(&initial_memo_category_filter));
        self.imp()
            .memo_preview_state
            .replace(initial_memo_preview_state.clone());

        let memo_sort = Rc::new(Cell::new(
            settings
                .as_ref()
                .map(|settings| MemoSort::from_setting(&settings.memo_sort_field()))
                .unwrap_or_default(),
        ));
        let memo_sort_descending = Rc::new(Cell::new(
            settings
                .as_ref()
                .map(|settings| settings.memo_sort_descending())
                .unwrap_or(true),
        ));
        let memo_source_filter = Rc::new(RefCell::new(
            settings
                .as_ref()
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
        let search_mode_model = gtk::StringList::new(&[
            "Alle Felder",
            "Zusammenfassung",
            "Beschreibung",
            "Erweitert",
        ]);
        let search_mode_selector =
            gtk::DropDown::new(Some(search_mode_model), None::<&gtk::Expression>);
        search_mode_selector.set_selected(memo_search_mode_index(initial_memo_search_mode));
        search_mode_selector.set_tooltip_text(Some(
            "Memo-Suchfeld: Zusammenfassung, Beschreibung, alle Felder oder summary:value AND description:value",
        ));
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
        let category_model = gtk::StringList::new(&["Alle Kategorien", "Ohne Kategorie"]);
        let category_selector =
            gtk::DropDown::new(Some(category_model.clone()), None::<&gtk::Expression>);
        category_selector.set_tooltip_text(Some("Notizkategorie auswählen"));
        self.imp()
            .memo_category_model
            .replace(Some(category_model.clone()));
        self.imp()
            .memo_category_selector
            .replace(Some(category_selector.clone()));
        set_category_selector_for_filter(&category_selector, &initial_memo_category_filter);
        search_entry.set_hexpand(true);
        sidebar_box.append(&search_entry);

        let sort_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_bottom(4)
            .build();
        sort_row.append(&search_mode_selector);
        sort_selector.set_hexpand(true);
        sort_row.append(&sort_selector);
        sort_row.append(&direction_button);
        sidebar_box.append(&sort_row);

        let memo_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Multiple)
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

        let page_weak_for_search_mode = self.downgrade();
        let memo_list_for_search_mode = memo_list.clone();
        let memo_records_for_search_mode = Rc::clone(&memo_records);
        let empty_state_for_search_mode = empty_state.clone();
        search_mode_selector.connect_selected_notify(move |selector| {
            let mode = memo_search_mode_from_index(selector.selected());
            if let Some(page) = page_weak_for_search_mode.upgrade() {
                page.imp().memo_search_mode.set(mode);
                if let Some(settings) = AppSettings::try_new() {
                    settings.set_memo_search_mode(mode.setting_value());
                }
                refresh_memo_filter_view(
                    &page,
                    &memo_list_for_search_mode,
                    &memo_records_for_search_mode,
                    &empty_state_for_search_mode,
                );
            }
        });

        let page_weak_for_search = self.downgrade();
        let empty_state_for_search = empty_state.clone();
        let memo_list_for_search = memo_list.clone();
        let memo_records_for_search = Rc::clone(&memo_records);
        search_entry.connect_search_changed(glib::clone!(
            #[strong]
            memo_list_for_search,
            move |entry| {
                // Advanced mode has a deliberately case-sensitive grammar
                // (`AND` and lower-case field names).  Field matching itself
                // remains case-insensitive in MemoFilter, so lower-casing the
                // complete query here only corrupts the grammar.
                let query = entry.text().trim().to_string();
                if let Some(page) = page_weak_for_search.upgrade() {
                    page.imp().memo_search_query.replace(query.clone());
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_search,
                        &memo_records_for_search,
                        &empty_state_for_search,
                    );
                }
            }
        ));

        let memo_source_filter_for_selector = Rc::clone(&memo_source_filter);
        let source_uids_for_selector = Rc::clone(&source_uids);
        let memo_source_list_for_selector = memo_source_list.clone();
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
            if let Some(page) = page_weak_for_source.upgrade() {
                refresh_memo_category_model(&page, &memo_list_for_source);
                refresh_memo_filter_view(
                    &page,
                    &memo_list_for_source,
                    &memo_records_for_source,
                    &empty_state_for_source,
                );
            }
        });

        let page_weak_for_category = self.downgrade();
        let category_model_for_selector = category_model.clone();
        let memo_list_for_category = memo_list.clone();
        let memo_records_for_category = Rc::clone(&memo_records);
        let empty_state_for_category = empty_state.clone();
        category_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_category.upgrade() else {
                return;
            };
            let category = if selector.selected() == 0 {
                None
            } else if selector.selected() == 1 {
                Some("without".to_string())
            } else {
                category_model_for_selector
                    .string(selector.selected())
                    .map(|value| value.to_string())
            };
            page.imp().memo_category_filter.replace(category.clone());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_category_filter(category.as_deref().unwrap_or("any"));
            }
            refresh_memo_filter_view(
                &page,
                &memo_list_for_category,
                &memo_records_for_category,
                &empty_state_for_category,
            );
        });

        let source_model_for_load = source_model.clone();
        let source_uids_for_load = Rc::clone(&source_uids);
        let source_selector_for_load = source_selector.clone();
        let memo_source_list_for_load = memo_source_list.clone();
        let page_weak_for_source_load = self.downgrade();
        let memo_list_for_source_load_actions = memo_list.clone();
        let memo_records_for_source_load_actions = Rc::clone(&memo_records);
        let empty_state_for_source_load_actions = empty_state.clone();
        let (source_sender, source_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let discovery =
                ECalService::new().discover_source_capabilities(ECalComponentType::Memo);
            if let Err(error) = source_sender.send_blocking(discovery) {
                tracing::debug!(%error, "Memo source discovery was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let Ok(discovery) = source_receiver.recv().await else {
                return;
            };
            let (sources, warning) = match discovery {
                Ok(discovery) => (
                    project_memo_source_capabilities(discovery.sources),
                    discovery.warning,
                ),
                Err(error) => (Vec::new(), Some(error.to_string())),
            };
            if let Some(warning) = warning {
                tracing::warn!(%warning, "Could not reach EDS memo lists");
            }

            let mut names = vec!["Alle Notizlisten".to_string()];
            let mut uids = Vec::new();
            let mut available_sources = HashSet::new();
            let mut unavailable_sources = HashSet::new();
            let mut writable_sources = HashSet::new();
            for source in sources {
                names.push(memo_source_projection_label(&source));
                if source.available {
                    available_sources.insert(source.uid.clone());
                } else {
                    unavailable_sources.insert(source.uid.clone());
                }
                if source.writable {
                    writable_sources.insert(source.uid.clone());
                }
                uids.push(source.uid);
            }
            if let Some(initial_source_filter) = initial_source_filter.as_deref()
                && !uids.iter().any(|uid| uid == initial_source_filter)
            {
                names.push(format!("Nicht verfügbare Quelle · {initial_source_filter}"));
                uids.push(initial_source_filter.to_string());
                unavailable_sources.insert(initial_source_filter.to_string());
            }
            if CalendarStoreService::new().is_writable() {
                names.push("Lokale Notizliste".to_string());
                uids.push("local-calendar".to_string());
                available_sources.insert("local-calendar".to_string());
                writable_sources.insert("local-calendar".to_string());
            }

            let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            source_uids_for_load.replace(uids.clone());
            if let Some(page) = page_weak_for_source_load.upgrade() {
                page.imp().memo_available_sources.replace(available_sources);
                page.imp()
                    .memo_unavailable_sources
                    .replace(unavailable_sources);
                page.imp().memo_writable_sources.replace(writable_sources);
                refresh_memo_filter_view(
                    &page,
                    &memo_list_for_source_load_actions,
                    &memo_records_for_source_load_actions,
                    &empty_state_for_source_load_actions,
                );
            }
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
            let source_model_for_settings = source_model.clone();
            let source_selector_for_settings = source_selector.clone();
            let memo_list_for_source_settings = memo_list.clone();
            let page_weak_for_source_settings = self.downgrade();
            settings.connect_changed(Some("memo-source-filter"), move |settings, _| {
                let raw_value = settings.string("memo-source-filter");
                let value = match normalize_memo_source_filter_value(&raw_value) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "Ignoring invalid live memo source filter");
                        String::new()
                    }
                };
                memo_source_filter_for_settings
                    .replace((!value.is_empty()).then_some(value.clone()));
                if let Some(page) = page_weak_for_source_settings.upgrade() {
                    page.imp()
                        .memo_source_filter
                        .replace((!value.is_empty()).then_some(value.clone()));
                }
                let mut selected = if value.is_empty() {
                    Some(0)
                } else {
                    source_uids_for_settings
                        .borrow()
                        .iter()
                        .position(|uid| uid == &value)
                        .map(|index| index as u32 + 1)
                };
                if selected.is_none() && !value.is_empty() {
                    let index = {
                        let mut uids = source_uids_for_settings.borrow_mut();
                        let index = uids.len() as u32 + 1;
                        uids.push(value.clone());
                        index
                    };
                    source_model_for_settings.append(&format!("Nicht verfügbare Quelle · {value}"));
                    selected = Some(index);
                    if let Some(page) = page_weak_for_source_settings.upgrade() {
                        page.imp()
                            .memo_unavailable_sources
                            .borrow_mut()
                            .insert(value.clone());
                    }
                }
                source_selector_for_settings.set_selected(selected.unwrap_or(0));
                if let Some(page) = page_weak_for_source_settings.upgrade() {
                    let Some(memo_records) = page.imp().memo_records.borrow().as_ref().cloned()
                    else {
                        return;
                    };
                    let Some(empty_state) = page.imp().memo_empty_state.borrow().as_ref().cloned()
                    else {
                        return;
                    };
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_source_settings,
                        &memo_records,
                        &empty_state,
                    );
                }
            });

            let search_mode_selector_for_settings = search_mode_selector.clone();
            let memo_list_for_search_mode_settings = memo_list.clone();
            let memo_records_for_search_mode_settings = Rc::clone(&memo_records);
            let empty_state_for_search_mode_settings = empty_state.clone();
            let page_weak_for_search_mode_settings = self.downgrade();
            settings.connect_changed(Some("memo-search-mode"), move |settings, _| {
                let mode = MemoSearchMode::from_setting(&settings.string("memo-search-mode"));
                search_mode_selector_for_settings.set_selected(memo_search_mode_index(mode));
                if let Some(page) = page_weak_for_search_mode_settings.upgrade() {
                    page.imp().memo_search_mode.set(mode);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_search_mode_settings,
                        &memo_records_for_search_mode_settings,
                        &empty_state_for_search_mode_settings,
                    );
                }
            });

            let category_selector_for_settings = category_selector.clone();
            let memo_list_for_category_settings = memo_list.clone();
            let memo_records_for_category_settings = Rc::clone(&memo_records);
            let empty_state_for_category_settings = empty_state.clone();
            let page_weak_for_category_settings = self.downgrade();
            settings.connect_changed(Some("memo-category-filter"), move |settings, _| {
                let value = settings.string("memo-category-filter").to_string();
                if let Some(page) = page_weak_for_category_settings.upgrade() {
                    page.imp()
                        .memo_category_filter
                        .replace(category_filter_option(&value));
                    refresh_memo_category_model(&page, &memo_list_for_category_settings);
                    set_category_selector_for_filter(&category_selector_for_settings, &value);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_category_settings,
                        &memo_records_for_category_settings,
                        &empty_state_for_category_settings,
                    );
                }
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

        let memo_surface_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .homogeneous(false)
            .build();
        let open_memo_btn = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Memo öffnen")
            .build();
        let forward_memo_btn = gtk::Button::builder()
            .icon_name("mail-forward-symbolic")
            .tooltip_text("Memo weiterleiten")
            .build();
        let open_url_memo_btn = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text("Memo-URL öffnen")
            .build();
        let print_memo_btn = gtk::Button::builder()
            .icon_name("document-print-symbolic")
            .tooltip_text("Memo drucken")
            .build();
        let save_as_memo_btn = gtk::Button::builder()
            .icon_name("document-save-as-symbolic")
            .tooltip_text("Memo speichern unter")
            .build();
        let copy_memo_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Memo kopieren")
            .build();
        let cut_memo_btn = gtk::Button::builder()
            .icon_name("edit-cut-symbolic")
            .tooltip_text("Memo ausschneiden")
            .build();
        let paste_memo_btn = gtk::Button::builder()
            .icon_name("edit-paste-symbolic")
            .tooltip_text("Memo einfügen")
            .build();
        let select_all_memo_btn = gtk::Button::builder()
            .icon_name("edit-select-all-symbolic")
            .tooltip_text("Alle Memos auswählen")
            .build();
        for (button, label) in [
            (&open_memo_btn, "Memo öffnen"),
            (&forward_memo_btn, "Memo weiterleiten"),
            (&open_url_memo_btn, "Memo-URL öffnen"),
            (&print_memo_btn, "Memo drucken"),
            (&save_as_memo_btn, "Memo speichern unter"),
            (&copy_memo_btn, "Memo kopieren"),
            (&cut_memo_btn, "Memo ausschneiden"),
            (&paste_memo_btn, "Memo einfügen"),
            (&select_all_memo_btn, "Alle Memos auswählen"),
        ] {
            button.add_css_class("flat");
            set_accessible_label(button, label);
        }
        for button in [
            &open_memo_btn,
            &forward_memo_btn,
            &open_url_memo_btn,
            &print_memo_btn,
            &save_as_memo_btn,
            &copy_memo_btn,
            &cut_memo_btn,
            &paste_memo_btn,
            &select_all_memo_btn,
        ] {
            memo_surface_actions.append(button);
        }
        self.imp().memo_surface_buttons.replace(Some(vec![
            save_memo_btn.clone(),
            export_memo_btn.clone(),
            delete_memo_btn.clone(),
            open_memo_btn.clone(),
            forward_memo_btn.clone(),
            open_url_memo_btn.clone(),
            print_memo_btn.clone(),
            save_as_memo_btn.clone(),
            copy_memo_btn.clone(),
            cut_memo_btn.clone(),
            paste_memo_btn.clone(),
            select_all_memo_btn.clone(),
        ]));
        content_box.append(&memo_surface_actions);

        let preview_controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let preview_toggle = gtk::ToggleButton::with_label("Vorschau");
        preview_toggle.set_active(initial_memo_preview_state.visible);
        let preview_orientation_model = gtk::StringList::new(&["Klassisch", "Vertikal"]);
        let preview_orientation_selector =
            gtk::DropDown::new(Some(preview_orientation_model), None::<&gtk::Expression>);
        preview_orientation_selector.set_selected(match initial_memo_preview_state.orientation {
            MemoPreviewOrientation::Classic => 0,
            MemoPreviewOrientation::Vertical => 1,
        });
        preview_orientation_selector.set_tooltip_text(Some("Ausrichtung der Memo-Vorschau"));
        preview_controls.append(&preview_toggle);
        preview_controls.append(&preview_orientation_selector);
        content_box.append(&preview_controls);

        let memo_preview_box = gtk::Box::builder()
            .orientation(match initial_memo_preview_state.orientation {
                MemoPreviewOrientation::Classic => gtk::Orientation::Vertical,
                MemoPreviewOrientation::Vertical => gtk::Orientation::Horizontal,
            })
            .spacing(6)
            .css_classes(vec!["card", "memo-preview"])
            .build();
        let memo_preview_label = gtk::Label::builder()
            .label("Keine eindeutige Memo-Auswahl")
            .use_markup(true)
            .wrap(true)
            .selectable(true)
            .xalign(0.0)
            .halign(gtk::Align::Fill)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        memo_preview_box.append(&memo_preview_label);
        memo_preview_box.set_visible(initial_memo_preview_state.visible);
        self.imp()
            .memo_preview_label
            .replace(Some(memo_preview_label.clone()));
        self.imp()
            .memo_preview_box
            .replace(Some(memo_preview_box.clone()));
        content_box.append(&memo_preview_box);

        if let Some(settings) = AppSettings::try_new() {
            let preview_toggle_for_settings = preview_toggle.clone();
            let memo_preview_box_for_settings = memo_preview_box.clone();
            let page_weak_for_preview_settings = self.downgrade();
            let memo_list_for_preview_settings = memo_list.clone();
            let memo_records_for_preview_settings = Rc::clone(&memo_records);
            settings.connect_changed(Some("memo-preview-visible"), move |settings, _| {
                let visible = settings.boolean("memo-preview-visible");
                preview_toggle_for_settings.set_active(visible);
                memo_preview_box_for_settings.set_visible(visible);
                if let Some(page) = page_weak_for_preview_settings.upgrade() {
                    page.imp()
                        .memo_preview_state
                        .borrow_mut()
                        .set_visible(visible);
                    refresh_memo_preview(
                        &page,
                        &memo_list_for_preview_settings,
                        &memo_records_for_preview_settings,
                    );
                }
            });

            let preview_orientation_selector_for_settings = preview_orientation_selector.clone();
            let memo_preview_box_for_orientation_settings = memo_preview_box.clone();
            let page_weak_for_orientation_settings = self.downgrade();
            let memo_list_for_orientation_settings = memo_list.clone();
            let memo_records_for_orientation_settings = Rc::clone(&memo_records);
            settings.connect_changed(Some("memo-preview-orientation"), move |settings, _| {
                let orientation = MemoPreviewOrientation::from_setting(
                    &settings.string("memo-preview-orientation"),
                );
                preview_orientation_selector_for_settings.set_selected(match orientation {
                    MemoPreviewOrientation::Classic => 0,
                    MemoPreviewOrientation::Vertical => 1,
                });
                memo_preview_box_for_orientation_settings.set_orientation(match orientation {
                    MemoPreviewOrientation::Classic => gtk::Orientation::Vertical,
                    MemoPreviewOrientation::Vertical => gtk::Orientation::Horizontal,
                });
                if let Some(page) = page_weak_for_orientation_settings.upgrade() {
                    page.imp()
                        .memo_preview_state
                        .borrow_mut()
                        .set_orientation(orientation);
                    refresh_memo_preview(
                        &page,
                        &memo_list_for_orientation_settings,
                        &memo_records_for_orientation_settings,
                    );
                }
            });
        }

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
        let memo_records_for_save = Rc::clone(&memo_records);
        let page_weak_for_save = self.downgrade();
        let memo_title_for_save = memo_title.clone();
        let memo_categories_for_save = memo_categories.clone();
        let memo_text_for_save = memo_text.clone();
        let memo_alarm_editor_for_save = memo_alarm_editor.clone();
        let memo_list_for_save = memo_list.clone();
        let empty_state_for_save = empty_state.clone();
        save_memo_btn.connect_clicked(move |_| {
            let Some(page) = page_weak_for_save.upgrade() else {
                return;
            };
            let mut selected =
                match selected_memo_records_checked(&memo_list_for_save, &memo_records_for_save) {
                    Ok(selected) if selected.len() == 1 => selected,
                    Ok(_) => {
                        show_toast_for_widget(
                            &memo_title_for_save,
                            "Speichern ist nur für genau eine Memo verfügbar",
                        );
                        return;
                    }
                    Err(error) => {
                        show_toast_for_widget(&memo_title_for_save, &error);
                        return;
                    }
                };
            let Some(memo) = selected.pop() else {
                show_toast_for_widget(&memo_title_for_save, "Keine Notiz ausgewählt");
                return;
            };
            if !memo_source_is_writable_for_page(&page, &memo.source_uid) {
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
            page.open_memo_dialog(
                memo_list_for_save.clone(),
                Rc::clone(&memo_records_for_save),
                empty_state_for_save.clone(),
                Some(staged),
            );
        });

        let memo_list_for_export = memo_list.clone();
        let memo_records_for_export = Rc::clone(&memo_records);
        export_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(
                &memo_list_for_export,
                &memo_records_for_export,
            ) {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_multi_export(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
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
                save_memo_file_async(
                    &button_for_result,
                    file,
                    payload,
                    "Memo(s) als iCalendar exportiert",
                );
            });
        });

        let memo_list_for_open = memo_list.clone();
        let records_for_open = Rc::clone(&memo_records);
        let empty_state_for_open = empty_state.clone();
        let page_weak_for_open = self.downgrade();
        open_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_open, &records_for_open) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(button, "Öffnen ist nur für genau eine Memo verfügbar");
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            let Some(page) = page_weak_for_open.upgrade() else {
                return;
            };
            page.open_memo_dialog(
                memo_list_for_open.clone(),
                Rc::clone(&records_for_open),
                empty_state_for_open.clone(),
                Some(memo),
            );
        });

        let memo_list_for_forward = memo_list.clone();
        let records_for_forward = Rc::clone(&memo_records);
        forward_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_forward, &records_for_forward) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(button, "Weiterleiten ist nur für genau eine Memo verfügbar");
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            match build_forward_publish_payload(&memo) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(
                        button,
                        "METHOD:PUBLISH bereitgestellt; echtes iTIP-Weiterleiten ist Linux-Handoff",
                    );
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_url = memo_list.clone();
        let records_for_url = Rc::clone(&memo_records);
        open_url_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(&memo_list_for_url, &records_for_url)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            if selected.len() != 1 {
                show_toast_for_widget(button, "URL öffnen ist nur für genau eine Memo verfügbar");
                return;
            }
            let memo = selected
                .into_iter()
                .next()
                .expect("selection length checked");
            match validated_url_for_record(&memo) {
                Ok(url) => {
                    let button_for_result = button.clone();
                    gio::AppInfo::launch_default_for_uri_async(
                        &url,
                        None::<&gio::AppLaunchContext>,
                        None::<&gio::Cancellable>,
                        move |result| match result {
                            Ok(()) => show_toast_for_widget(&button_for_result, "URL geöffnet"),
                            Err(error) => show_toast_for_widget(
                                &button_for_result,
                                &format!("URL konnte nicht geöffnet werden: {error}"),
                            ),
                        },
                    );
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_print = memo_list.clone();
        let records_for_print = Rc::clone(&memo_records);
        print_memo_btn.connect_clicked(move |button| {
            let selected = match selected_memo_records_checked(&memo_list_for_print, &records_for_print)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            if selected.len() != 1 {
                show_toast_for_widget(button, "Drucken ist nur für genau eine Memo verfügbar");
                return;
            }
            let memo = selected.into_iter().next().expect("selection length checked");
            match build_print_document(&memo) {
                Ok(document) => show_toast_for_widget(
                    button,
                    &format!(
                        "Portables Druckdokument vorbereitet ({} Zeichen); Print-Portal ist Linux-Handoff",
                        document.html.len()
                    ),
                ),
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let memo_list_for_save_as = memo_list.clone();
        let records_for_save_as = Rc::clone(&memo_records);
        save_as_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_save_as, &records_for_save_as) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            if selected.len() != 1 {
                show_toast_for_widget(
                    button,
                    "Speichern unter ist nur für genau eine Memo verfügbar",
                );
                return;
            }
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_multi_export(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
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
                .title("Memo als iCalendar speichern")
                .initial_name("memos.ics")
                .modal(true)
                .build();
            let button_for_result = button.clone();
            file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
                let Ok(file) = result else {
                    return;
                };
                save_memo_file_async(&button_for_result, file, payload, "Memo(s) gespeichert");
            });
        });

        let memo_list_for_copy = memo_list.clone();
        let records_for_copy = Rc::clone(&memo_records);
        copy_memo_btn.connect_clicked(move |button| {
            let selected =
                match selected_memo_records_checked(&memo_list_for_copy, &records_for_copy) {
                    Ok(selected) => selected,
                    Err(error) => {
                        show_toast_for_widget(button, &error);
                        return;
                    }
                };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            match build_clipboard_payload(&selected, &identities) {
                Ok(payload) => {
                    button.display().clipboard().set_text(&payload);
                    show_toast_for_widget(button, "Memo(s) in die Zwischenablage kopiert");
                }
                Err(error) => show_toast_for_widget(button, &error),
            }
        });

        let cut_payload = Rc::new(RefCell::new(None::<String>));
        let copy_for_cut = copy_memo_btn.clone();
        let delete_for_cut = delete_memo_btn.clone();
        let memo_list_for_cut = memo_list.clone();
        let records_for_cut = Rc::clone(&memo_records);
        let cut_payload_for_cut = Rc::clone(&cut_payload);
        cut_memo_btn.connect_clicked(move |button| {
            if !copy_for_cut.is_sensitive() || !delete_for_cut.is_sensitive() {
                show_toast_for_widget(button, "Ausschneiden ist für diese Auswahl nicht verfügbar");
                return;
            }
            let selected = match selected_memo_records_checked(&memo_list_for_cut, &records_for_cut)
            {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let payload = match build_clipboard_payload(&selected, &identities) {
                Ok(payload) => payload,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            cut_payload_for_cut.replace(Some(payload));
            delete_for_cut.activate();
        });

        let memo_list_for_select_all = memo_list.clone();
        select_all_memo_btn.connect_clicked(move |_| {
            memo_list_for_select_all.select_all();
        });

        let page_weak_for_paste = self.downgrade();
        let memo_list_for_paste = memo_list.clone();
        let records_for_paste = Rc::clone(&memo_records);
        let empty_state_for_paste = empty_state.clone();
        paste_memo_btn.connect_clicked(move |button| {
            let Some(page) = page_weak_for_paste.upgrade() else {
                return;
            };
            let destination = page
                .imp()
                .memo_source_filter
                .borrow()
                .clone()
                .unwrap_or_else(|| "local-calendar".to_string());
            if !memo_source_is_writable_for_page(&page, &destination) {
                show_toast_for_widget(button, "Keine schreibfähige Zielquelle ausgewählt");
                return;
            }
            let clipboard = button.display().clipboard();
            let button_for_result = button.clone();
            let page_weak_for_result = page_weak_for_paste.clone();
            let page_weak_for_clipboard = page_weak_for_paste.clone();
            let memo_list_for_result = memo_list_for_paste.clone();
            let records_for_result = Rc::clone(&records_for_paste);
            let records_for_clipboard = Rc::clone(&records_for_paste);
            let empty_state_for_result = empty_state_for_paste.clone();
            clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
                let payload = match result {
                    Ok(Some(payload)) => payload.to_string(),
                    Ok(None) => {
                        show_toast_for_widget(
                            &button_for_result,
                            "Die Zwischenablage enthält keinen Text",
                        );
                        return;
                    }
                    Err(error) => {
                        show_toast_for_widget(
                            &button_for_result,
                            &format!("Zwischenablage nicht lesbar: {error}"),
                        );
                        return;
                    }
                };
                let Some(page) = page_weak_for_clipboard.upgrade() else {
                    return;
                };
                let current_destination = page
                    .imp()
                    .memo_source_filter
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "local-calendar".to_string());
                if current_destination != destination
                    || !memo_source_is_writable_for_page(&page, &destination)
                {
                    show_toast_for_widget(
                        &button_for_result,
                        "Die Zielquelle hat sich geändert oder ist nicht mehr schreibfähig",
                    );
                    return;
                }
                let existing = records_for_clipboard.borrow().clone();
                let plan = match plan_paste_into(&payload, &destination, true, &existing) {
                    Ok(plan) => plan,
                    Err(error) => {
                        show_toast_for_widget(&button_for_result, &error);
                        return;
                    }
                };
                let (sender, receiver) = async_channel::bounded(1);
                std::thread::spawn(move || {
                    let result = execute_memo_paste_batch(plan);
                    if let Err(error) = sender.send_blocking(result) {
                        tracing::debug!(%error, "Memo paste result was dropped");
                    }
                });
                glib::spawn_future_local(async move {
                    let Ok(result) = receiver.recv().await else {
                        return;
                    };
                    match result {
                        Ok(successes) => {
                            for (_, memo) in successes {
                                upsert_memo_row(
                                    &memo_list_for_result,
                                    Rc::clone(&records_for_result),
                                    memo,
                                );
                            }
                            if let Some(page) = page_weak_for_result.upgrade() {
                                let generation =
                                    page.imp().memo_refresh_generation.get().wrapping_add(1);
                                page.imp().memo_refresh_generation.set(generation);
                                refresh_memo_category_model(&page, &memo_list_for_result);
                                refresh_memo_filter_view(
                                    &page,
                                    &memo_list_for_result,
                                    &records_for_result,
                                    &empty_state_for_result,
                                );
                            }
                            show_toast_for_widget(&button_for_result, "Memo(s) eingefügt");
                        }
                        Err(error) => {
                            if let Some(page) = page_weak_for_result.upgrade() {
                                reload_memo_snapshot_async(
                                    &page,
                                    &memo_list_for_result,
                                    &records_for_result,
                                    &empty_state_for_result,
                                );
                            }
                            show_toast_for_widget(&button_for_result, &error);
                        }
                    }
                });
            });
        });

        let memo_records_for_delete = Rc::clone(&memo_records);
        let page_weak_for_delete = self.downgrade();
        let memo_list_for_delete = memo_list.clone();
        let empty_state_for_delete = empty_state.clone();
        let memo_title_for_delete = memo_title.clone();
        let memo_mutation_generation_for_delete = Rc::clone(&memo_mutation_generation);
        let cut_payload_for_delete = Rc::clone(&cut_payload);
        delete_memo_btn.connect_clicked(move |button| {
            let pending_cut_payload = cut_payload_for_delete.borrow_mut().take();
            let Some(page) = page_weak_for_delete.upgrade() else {
                return;
            };
            let selected = match selected_memo_records_checked(
                &memo_list_for_delete,
                &memo_records_for_delete,
            ) {
                Ok(selected) => selected,
                Err(error) => {
                    show_toast_for_widget(button, &error);
                    return;
                }
            };
            let identities = selected.iter().map(MemoIdentity::from).collect::<Vec<_>>();
            let planned = match plan_delete(&selected, &identities, |source_uid| {
                memo_source_is_writable_for_page(&page, source_uid)
            }) {
                Ok(planned) => planned,
                Err(result) => {
                    show_toast_for_widget(button, &result.visible_error_text());
                    return;
                }
            };
            if planned.is_empty() {
                show_toast_for_widget(button, "Keine Memo ausgewählt");
                return;
            }
            let confirmation_message = if planned.len() == 1 {
                "Die ausgewählte Notiz wird dauerhaft entfernt."
            } else {
                "Die ausgewählten Notizen werden dauerhaft entfernt."
            };
            let delete_generation = memo_mutation_generation_for_delete.get();
            let delete_refresh_generation = page.imp().memo_refresh_generation.get();
            let page_weak_for_action = page_weak_for_delete.clone();
            let page_weak_for_guard = page_weak_for_action.clone();
            let memo_list_for_action = memo_list_for_delete.clone();
            let memo_records_for_action = Rc::clone(&memo_records_for_delete);
            let empty_state_for_action = empty_state_for_delete.clone();
            let memo_mutation_generation_for_action =
                Rc::clone(&memo_mutation_generation_for_delete);
            let button_for_action = button.clone();
            crate::dialogs::run_destructive_action(
                &memo_title_for_delete,
                "Notizen löschen?",
                confirmation_message,
                "Löschen",
                move || {
                    if memo_mutation_generation_for_action.get() != delete_generation {
                        show_toast_for_widget(
                            &button_for_action,
                            "Die Memo-Auswahl ist inzwischen veraltet",
                        );
                        return;
                    }
                    let Some(page) = page_weak_for_guard.upgrade() else {
                        return;
                    };
                    if page.imp().memo_refresh_generation.get() != delete_refresh_generation {
                        show_toast_for_widget(
                            &button_for_action,
                            "Der Memo-Bestand wurde inzwischen aktualisiert",
                        );
                        return;
                    }
                    let (sender, receiver) = async_channel::bounded(1);
                    std::thread::spawn(move || {
                        let result = execute_memo_delete_batch(planned);
                        if let Err(error) = sender.send_blocking(result) {
                            tracing::debug!(%error, "Memo deletion batch result was dropped");
                        }
                    });
                    let page_weak_for_result = page_weak_for_action.clone();
                    let memo_list_for_result = memo_list_for_action.clone();
                    let memo_records_for_result = Rc::clone(&memo_records_for_action);
                    let empty_state_for_result = empty_state_for_action.clone();
                    let button_for_result = button_for_action.clone();
                    glib::spawn_future_local(async move {
                        let Ok(result) = receiver.recv().await else {
                            return;
                        };
                        match result {
                            Ok(identities) => {
                                for identity in identities {
                                    remove_memo_row(
                                        &memo_list_for_result,
                                        Rc::clone(&memo_records_for_result),
                                        &identity.source_uid,
                                        &identity.uid,
                                    );
                                }
                                if let Some(payload) = pending_cut_payload {
                                    button_for_result.display().clipboard().set_text(&payload);
                                }
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    let generation =
                                        page.imp().memo_refresh_generation.get().wrapping_add(1);
                                    page.imp().memo_refresh_generation.set(generation);
                                    refresh_memo_category_model(&page, &memo_list_for_result);
                                    refresh_memo_filter_view(
                                        &page,
                                        &memo_list_for_result,
                                        &memo_records_for_result,
                                        &empty_state_for_result,
                                    );
                                }
                                show_toast_for_widget(&button_for_result, "Memo(s) gelöscht");
                            }
                            Err(error) => {
                                if let Some(page) = page_weak_for_result.upgrade() {
                                    reload_memo_snapshot_async(
                                        &page,
                                        &memo_list_for_result,
                                        &memo_records_for_result,
                                        &empty_state_for_result,
                                    );
                                }
                                show_toast_for_widget(&button_for_result, &error);
                            }
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

        let memo_key_controller = gtk::EventControllerKey::new();
        let open_for_key = open_memo_btn.clone();
        let copy_for_key = copy_memo_btn.clone();
        let cut_for_key = cut_memo_btn.clone();
        let paste_for_key = paste_memo_btn.clone();
        let delete_for_key = delete_memo_btn.clone();
        let select_all_for_key = select_all_memo_btn.clone();
        memo_key_controller.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                || modifiers.contains(gtk::gdk::ModifierType::META_MASK);
            if control && key == gtk::gdk::Key::A {
                select_all_for_key.activate();
                return glib::Propagation::Stop;
            }
            if control && key == gtk::gdk::Key::C {
                copy_for_key.activate();
                return glib::Propagation::Stop;
            }
            if control && key == gtk::gdk::Key::X {
                cut_for_key.activate();
                return glib::Propagation::Stop;
            }
            if control && key == gtk::gdk::Key::V {
                paste_for_key.activate();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Delete {
                delete_for_key.activate();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Return {
                open_for_key.activate();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        memo_list.add_controller(memo_key_controller);

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

        let page_weak_for_surface_selection = self.downgrade();
        let memo_list_for_surface_selection = memo_list.clone();
        let memo_records_for_surface_selection = Rc::clone(&memo_records);
        let save_memo_btn_for_surface_selection = save_memo_btn.clone();
        let export_memo_btn_for_surface_selection = export_memo_btn.clone();
        let delete_memo_btn_for_surface_selection = delete_memo_btn.clone();
        let open_memo_btn_for_surface_selection = open_memo_btn.clone();
        let forward_memo_btn_for_surface_selection = forward_memo_btn.clone();
        let open_url_memo_btn_for_surface_selection = open_url_memo_btn.clone();
        let print_memo_btn_for_surface_selection = print_memo_btn.clone();
        let save_as_memo_btn_for_surface_selection = save_as_memo_btn.clone();
        let copy_memo_btn_for_surface_selection = copy_memo_btn.clone();
        let cut_memo_btn_for_surface_selection = cut_memo_btn.clone();
        let paste_memo_btn_for_surface_selection = paste_memo_btn.clone();
        let select_all_memo_btn_for_surface_selection = select_all_memo_btn.clone();
        memo_list.connect_selected_rows_changed(move |_| {
            let Some(page) = page_weak_for_surface_selection.upgrade() else {
                return;
            };
            sync_memo_surface_selection(&page, &memo_list_for_surface_selection);
            update_memo_surface_widgets(
                &page,
                &memo_list_for_surface_selection,
                &memo_records_for_surface_selection,
                &save_memo_btn_for_surface_selection,
                &export_memo_btn_for_surface_selection,
                &delete_memo_btn_for_surface_selection,
                &open_memo_btn_for_surface_selection,
                &forward_memo_btn_for_surface_selection,
                &open_url_memo_btn_for_surface_selection,
                &print_memo_btn_for_surface_selection,
                &save_as_memo_btn_for_surface_selection,
                &copy_memo_btn_for_surface_selection,
                &cut_memo_btn_for_surface_selection,
                &paste_memo_btn_for_surface_selection,
                &select_all_memo_btn_for_surface_selection,
            );
        });

        let page_weak_for_preview_toggle = self.downgrade();
        let memo_list_for_preview_toggle = memo_list.clone();
        let memo_records_for_preview_toggle = Rc::clone(&memo_records);
        let memo_preview_box_for_toggle = memo_preview_box.clone();
        preview_toggle.connect_toggled(move |toggle| {
            let Some(page) = page_weak_for_preview_toggle.upgrade() else {
                return;
            };
            page.imp()
                .memo_preview_state
                .borrow_mut()
                .set_visible(toggle.is_active());
            memo_preview_box_for_toggle.set_visible(toggle.is_active());
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_preview_visible(toggle.is_active());
            }
            refresh_memo_preview(
                &page,
                &memo_list_for_preview_toggle,
                &memo_records_for_preview_toggle,
            );
        });

        let page_weak_for_preview_orientation = self.downgrade();
        let memo_list_for_preview_orientation = memo_list.clone();
        let memo_records_for_preview_orientation = Rc::clone(&memo_records);
        let memo_preview_box_for_orientation = memo_preview_box.clone();
        preview_orientation_selector.connect_selected_notify(move |selector| {
            let Some(page) = page_weak_for_preview_orientation.upgrade() else {
                return;
            };
            let orientation = if selector.selected() == 1 {
                MemoPreviewOrientation::Vertical
            } else {
                MemoPreviewOrientation::Classic
            };
            page.imp()
                .memo_preview_state
                .borrow_mut()
                .set_orientation(orientation);
            memo_preview_box_for_orientation.set_orientation(match orientation {
                MemoPreviewOrientation::Classic => gtk::Orientation::Vertical,
                MemoPreviewOrientation::Vertical => gtk::Orientation::Horizontal,
            });
            if let Some(settings) = AppSettings::try_new() {
                settings.set_memo_preview_orientation(orientation.setting_value());
            }
            refresh_memo_preview(
                &page,
                &memo_list_for_preview_orientation,
                &memo_records_for_preview_orientation,
            );
        });

        let memo_text_for_activate = memo_text.clone();
        memo_list.connect_row_activated(move |_, _| {
            memo_text_for_activate.grab_focus();
        });

        let memo_list_for_load = memo_list.clone();
        let empty_state_for_load = empty_state.clone();
        let memo_meta_for_load = memo_meta.clone();
        let memo_records_for_load = Rc::clone(&memo_records);
        let page_weak_for_load = self.downgrade();
        let load_generation = self.imp().memo_refresh_generation.get().wrapping_add(1);
        self.imp().memo_refresh_generation.set(load_generation);
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
                    let Some(page) = page_weak_for_load.upgrade() else {
                        return;
                    };
                    if page.imp().memo_refresh_generation.get() != load_generation {
                        tracing::debug!(
                            load_generation,
                            current_generation = page.imp().memo_refresh_generation.get(),
                            "Discarding stale memo snapshot"
                        );
                        return;
                    }
                    if let Some(warning) = result.warning {
                        tracing::warn!(%warning, "Memo data loaded with a backend warning");
                    }
                    let memos = result.snapshot.memos;
                    let memo_count = memos.len();
                    replace_memo_snapshot(
                        &memo_list_for_load,
                        Rc::clone(&memo_records_for_load),
                        memos,
                    );
                    if memo_count == 0 {
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
                        memo_meta_for_load
                            .set_label(&format!("{} Notiz(en) aus {origin} geladen", memo_count));
                    }
                    refresh_memo_category_model(&page, &memo_list_for_load);
                    refresh_memo_filter_view(
                        &page,
                        &memo_list_for_load,
                        &memo_records_for_load,
                        &empty_state_for_load,
                    );
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
                    if matches!(&event, ECalSyncEvent::Change(_)) {
                        if let Some(page) = page_weak_for_monitor.upgrade() {
                            let generation =
                                page.imp().memo_refresh_generation.get().wrapping_add(1);
                            page.imp().memo_refresh_generation.set(generation);
                        }
                    }
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
                        refresh_memo_filter_view(
                            &page,
                            &memo_list_for_monitor,
                            &memo_records_for_monitor,
                            &empty_state_for_monitor,
                        );
                    }
                }
            });
        }

        update_memo_surface_widgets(
            self,
            &memo_list,
            &memo_records,
            &save_memo_btn,
            &export_memo_btn,
            &delete_memo_btn,
            &open_memo_btn,
            &forward_memo_btn,
            &open_url_memo_btn,
            &print_memo_btn,
            &save_as_memo_btn,
            &copy_memo_btn,
            &cut_memo_btn,
            &paste_memo_btn,
            &select_all_memo_btn,
        );

        let content_page = adw::NavigationPage::builder()
            .title("Notiz-Details")
            .child(&content_box)
            .build();

        split.set_content(Some(&content_page));
        let split_for_navigation = split.clone();
        let memo_list_for_navigation = memo_list.clone();
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
                memo_list_for_navigation.unselect_all();
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
        // The source list is authoritative only after discovery; an edited
        // memo from a missing source must remain visible but never writable.
        save_button.set_sensitive(false);
        let edit_source_uid_for_load = edit_source_uid.clone();
        let preferred_source_uid_for_load = preferred_source_uid.clone();
        let page_weak_for_dialog_source_load = self.downgrade();
        let dialog_weak_for_dialog_source_load = dialog.downgrade();
        let (dialog_source_sender, dialog_source_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let discovery =
                ECalService::new().discover_source_capabilities(ECalComponentType::Memo);
            if let Err(error) = dialog_source_sender.send_blocking(discovery) {
                tracing::debug!(%error, "Memo dialog source discovery was dropped");
            }
        });
        glib::spawn_future_local(async move {
            let Ok(discovery) = dialog_source_receiver.recv().await else {
                return;
            };
            let Some(dialog) = dialog_weak_for_dialog_source_load.upgrade() else {
                return;
            };
            if !dialog.is_visible() {
                return;
            }
            let (sources, warning, discovery_failed) = match discovery {
                Ok(discovery) => (
                    project_memo_source_capabilities(discovery.sources),
                    discovery.warning,
                    false,
                ),
                Err(error) => (Vec::new(), Some(error.to_string()), true),
            };
            if let Some(warning) = warning {
                tracing::warn!(%warning, "Could not reach EDS memo lists for memo dialog");
            }
            let mut available_sources = HashSet::new();
            let mut unavailable_sources = HashSet::new();
            let mut writable_sources = HashSet::new();
            let mut names = Vec::new();
            let mut uids = source_uids_for_load.borrow_mut();
            uids.clear();
            for source in sources {
                names.push(memo_source_projection_label(&source));
                if source.available {
                    available_sources.insert(source.uid.clone());
                } else {
                    unavailable_sources.insert(source.uid.clone());
                }
                if source.writable {
                    writable_sources.insert(source.uid.clone());
                }
                uids.push(source.uid);
            }
            let preserved_source_uid = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .filter(|uid| {
                    *uid != "local-calendar" && !uids.iter().any(|candidate| candidate == uid)
                });
            if let Some(uid) = preserved_source_uid {
                names.push(format!("Nicht verfügbare Quelle · {uid}"));
                uids.push(uid.to_string());
                unavailable_sources.insert(uid.to_string());
            }
            if local_calendar_available {
                names.push("Lokale Notizliste".to_string());
                uids.push("local-calendar".to_string());
                available_sources.insert("local-calendar".to_string());
                writable_sources.insert("local-calendar".to_string());
            }
            if names.is_empty() {
                names.push(if discovery_failed {
                    "Notizlisten sind nicht erreichbar".to_string()
                } else {
                    "Keine aktive Notizliste verfügbar".to_string()
                });
            }
            let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
            source_model_for_load.splice(0, source_model_for_load.n_items(), &name_refs);
            if let Some(page) = page_weak_for_dialog_source_load.upgrade() {
                page.imp().memo_available_sources.replace(available_sources);
                page.imp()
                    .memo_unavailable_sources
                    .replace(unavailable_sources);
                page.imp().memo_writable_sources.replace(writable_sources);
            }
            let selected_index = edit_source_uid_for_load
                .as_deref()
                .or(preferred_source_uid_for_load.as_deref())
                .and_then(|uid| uids.iter().position(|candidate| candidate == uid))
                .or_else(|| (!uids.is_empty()).then_some(0));
            let selected_source_uid = selected_index.and_then(|index| uids.get(index).cloned());
            drop(uids);
            if let Some(index) = selected_index {
                source_row_for_load.set_selected(index as u32);
                let writable = page_weak_for_dialog_source_load
                    .upgrade()
                    .is_some_and(|page| {
                        selected_source_uid
                            .as_deref()
                            .is_some_and(|uid| memo_source_is_writable_for_page(&page, uid))
                    });
                save_button_for_load.set_sensitive(writable);
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
            let Some(page) = page_weak.upgrade() else {
                return;
            };
            if !memo_source_is_writable_for_page(&page, &source_uid) {
                show_toast_for_widget(
                    &dialog,
                    "Die ausgewählte Notizliste ist nicht schreibfähig oder nicht verfügbar",
                );
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
                        if let Some(page) = page_weak_for_result.upgrade() {
                            let generation =
                                page.imp().memo_refresh_generation.get().wrapping_add(1);
                            page.imp().memo_refresh_generation.set(generation);
                        }
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

fn replace_memo_snapshot(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    memos: Vec<MemoRecord>,
) {
    let selection = memo_selection_from_rows(memo_list);
    memo_list.unselect_all();
    while let Some(child) = memo_list.first_child() {
        memo_list.remove(&child);
    }
    memo_records.replace(memos);
    for memo in memo_records.borrow().iter() {
        append_memo_row(memo_list, memo);
    }
    restore_memo_selection_rows(memo_list, &selection);
}

fn upsert_memo_row(
    memo_list: &gtk::ListBox,
    memo_records: Rc<RefCell<Vec<MemoRecord>>>,
    memo: MemoRecord,
) {
    let selection = memo_selection_from_rows(memo_list);
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
    restore_memo_selection_rows(memo_list, &selection);
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
        row.set_data("memo-summary", memo.summary.clone());
        row.set_data("memo-description", memo.description.clone());
        row.set_data("memo-component", memo.component.clone());
        row.set_data("memo-source-display", memo.source_display_name.clone());
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
    if let Some(filter) = current_filter.as_deref()
        && !filter.eq_ignore_ascii_case("without")
        && !categories
            .iter()
            .any(|category| category.eq_ignore_ascii_case(filter))
    {
        categories.push(filter.to_string());
        categories.sort_by_cached_key(|category| category.to_lowercase());
    }

    let mut names = vec!["Alle Kategorien".to_string(), "Ohne Kategorie".to_string()];
    names.extend(categories);
    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &name_refs);
    let selected = current_filter
        .as_deref()
        .map(|filter| {
            if filter.eq_ignore_ascii_case("without") {
                1
            } else {
                names
                    .iter()
                    .position(|category| category.eq_ignore_ascii_case(filter))
                    .unwrap_or(0) as u32
            }
        })
        .unwrap_or(0);
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
    memo_matches_filter_with_mode(
        memo,
        source_filter,
        category_filter,
        query,
        MemoSearchMode::Any,
    )
}

fn memo_matches_filter_with_mode(
    memo: &MemoRecord,
    source_filter: Option<&str>,
    category_filter: Option<&str>,
    query: &str,
    search_mode: MemoSearchMode,
) -> bool {
    let category = category_filter
        .map(MemoCategoryFilter::from_setting)
        .unwrap_or_default();
    MemoFilter::new(
        search_mode,
        query.to_string(),
        category,
        source_filter.map(ToOwned::to_owned),
    )
    .is_ok_and(|filter| filter.matches(memo))
}

fn apply_memo_list_filter(
    memo_list: &gtk::ListBox,
    source_filter: Option<String>,
    category_filter: Option<String>,
    query: String,
) {
    apply_memo_list_filter_with_mode(
        memo_list,
        source_filter,
        category_filter,
        query,
        MemoSearchMode::Any,
    );
}

fn apply_memo_list_filter_with_mode(
    memo_list: &gtk::ListBox,
    source_filter: Option<String>,
    category_filter: Option<String>,
    query: String,
    search_mode: MemoSearchMode,
) {
    memo_list.set_filter_func(move |row| {
        let Some(memo) = memo_record_from_row(row) else {
            return false;
        };
        memo_matches_filter_with_mode(
            &memo,
            source_filter.as_deref(),
            category_filter.as_deref(),
            &query,
            search_mode,
        )
    });
}

fn memo_record_from_row(row: &gtk::ListBoxRow) -> Option<MemoRecord> {
    unsafe {
        Some(MemoRecord {
            uid: row.data::<String>("memo-id")?.as_ref().clone(),
            source_uid: row.data::<String>("memo-source-uid")?.as_ref().clone(),
            source_display_name: row
                .data::<String>("memo-source-display")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            component: row.data::<String>("memo-component")?.as_ref().clone(),
            summary: row
                .data::<String>("memo-summary")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            description: row
                .data::<String>("memo-description")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
            updated: row
                .data::<String>("memo-sort-updated")
                .map(|value| value.as_ref().clone())
                .unwrap_or_default(),
        })
    }
}

fn category_filter_option(value: &str) -> Option<String> {
    match MemoCategoryFilter::from_setting(value) {
        MemoCategoryFilter::Any => None,
        MemoCategoryFilter::Without => Some("without".to_string()),
        MemoCategoryFilter::Named(value) => Some(value),
    }
}

fn set_category_selector_for_filter(selector: &gtk::DropDown, value: &str) {
    let selected = match MemoCategoryFilter::from_setting(value) {
        MemoCategoryFilter::Any => 0,
        MemoCategoryFilter::Without => 1,
        MemoCategoryFilter::Named(_) => {
            let Some(model) = selector.model().and_downcast::<gtk::StringList>() else {
                return;
            };
            (0..model.n_items())
                .find(|index| {
                    model
                        .string(*index)
                        .is_some_and(|name| name.eq_ignore_ascii_case(value))
                })
                .unwrap_or(0)
        }
    };
    selector.set_selected(selected);
}

fn memo_search_mode_index(mode: MemoSearchMode) -> u32 {
    match mode {
        MemoSearchMode::Any => 0,
        MemoSearchMode::Summary => 1,
        MemoSearchMode::Description => 2,
        MemoSearchMode::Advanced => 3,
    }
}

fn memo_search_mode_from_index(index: u32) -> MemoSearchMode {
    match index {
        1 => MemoSearchMode::Summary,
        2 => MemoSearchMode::Description,
        3 => MemoSearchMode::Advanced,
        _ => MemoSearchMode::Any,
    }
}

fn refresh_memo_filter_view(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    empty_state: &gtk::Label,
) {
    let source_filter = page.imp().memo_source_filter.borrow().clone();
    let category_filter = page.imp().memo_category_filter.borrow().clone();
    let query = page.imp().memo_search_query.borrow().clone();
    let search_mode = page.imp().memo_search_mode.get();
    let category = category_filter
        .as_deref()
        .map(MemoCategoryFilter::from_setting)
        .unwrap_or_default();
    let filter = MemoFilter::new(search_mode, query.clone(), category, source_filter.clone());
    apply_memo_list_filter_with_mode(
        memo_list,
        source_filter.clone(),
        category_filter.clone(),
        query.clone(),
        search_mode,
    );

    match filter {
        Err(error) => {
            empty_state.set_label(&format!("Ungültige erweiterte Suche: {error}"));
            empty_state.set_visible(true);
        }
        Ok(filter) => {
            let has_records = filter.filter(&memo_records.borrow()).first().is_some();
            empty_state.set_label(
                if query.is_empty() && source_filter.is_none() && category_filter.is_none() {
                    "Keine synchronisierten Notizen geladen"
                } else {
                    "Keine passenden Notizen"
                },
            );
            empty_state.set_visible(!has_records);
        }
    }
    memo_list.invalidate_filter();
    restore_memo_selection_state(
        page,
        memo_list,
        memo_records,
        page.imp().memo_refresh_generation.get(),
    );
    sync_memo_surface_selection(page, memo_list);
    update_memo_surface_widgets_from_page(page, memo_list, memo_records);
}

fn restore_memo_selection_state(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    generation: u64,
) {
    let records = memo_records.borrow().clone();
    let mut selection = page.imp().memo_selection.borrow().clone();
    selection.restore_after_refresh(&records, generation);
    page.imp().memo_selection.replace(selection.clone());
    restore_memo_selection_rows(memo_list, &selection);
}

fn memo_selection_identities(memo_list: &gtk::ListBox) -> Vec<MemoIdentity> {
    memo_list
        .selected_rows()
        .iter()
        .filter_map(|row| unsafe {
            Some(MemoIdentity::new(
                row.data::<String>("memo-source-uid")?.as_ref().clone(),
                row.data::<String>("memo-id")?.as_ref().clone(),
            ))
        })
        .collect()
}

fn memo_selection_from_rows(memo_list: &gtk::ListBox) -> MemoSelection {
    let mut selection = MemoSelection::default();
    selection.set_selected(memo_selection_identities(memo_list));
    let focus = memo_list.selected_row().and_then(|row| unsafe {
        Some(MemoIdentity::new(
            row.data::<String>("memo-source-uid")?.as_ref().clone(),
            row.data::<String>("memo-id")?.as_ref().clone(),
        ))
    });
    selection.set_focus(focus);
    selection
}

fn selected_memo_records(
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) -> Vec<MemoRecord> {
    let identities = memo_selection_identities(memo_list);
    let records = memo_records.borrow();
    identities
        .iter()
        .filter_map(|identity| {
            records
                .iter()
                .find(|memo| MemoIdentity::from(*memo) == *identity)
                .cloned()
        })
        .collect()
}

fn selected_memo_records_checked(
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) -> Result<Vec<MemoRecord>, String> {
    let identities = memo_selection_identities(memo_list);
    let records = memo_records.borrow();
    if let Some(identity) = identities.iter().find(|identity| {
        records
            .iter()
            .filter(|record| MemoIdentity::from(*record) == **identity)
            .count()
            != 1
    }) {
        return Err(format!(
            "Die Memo-Daten enthalten keine eindeutige Identität {} / {}",
            identity.source_uid, identity.uid
        ));
    }
    drop(records);
    let selected = selected_memo_records(memo_list, memo_records);
    if selected.len() != identities.len() {
        return Err("Die Memo-Auswahl ist nicht mehr aktuell; bitte neu auswählen".to_string());
    }
    if selected.is_empty() {
        return Err("Keine Memo ausgewählt".to_string());
    }
    Ok(selected)
}

fn sync_memo_surface_selection(page: &MemosPage, memo_list: &gtk::ListBox) {
    let identities = memo_selection_identities(memo_list);
    let focus = memo_list.selected_row().and_then(|row| unsafe {
        Some(MemoIdentity::new(
            row.data::<String>("memo-source-uid")?.as_ref().clone(),
            row.data::<String>("memo-id")?.as_ref().clone(),
        ))
    });
    let mut selection = page.imp().memo_selection.borrow_mut();
    selection.set_selected(identities);
    selection.set_generation(page.imp().memo_refresh_generation.get());
    if focus.is_some() {
        selection.set_focus(focus);
    }
}

fn restore_memo_selection_rows(memo_list: &gtk::ListBox, selection: &MemoSelection) {
    if selection.is_empty() {
        memo_list.unselect_all();
        return;
    }
    memo_list.unselect_all();
    let focus = selection.focus().cloned();
    for identity in selection
        .selected()
        .iter()
        .filter(|identity| focus.as_ref() != Some(*identity))
    {
        let mut child = memo_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            if !row.is_visible() {
                continue;
            }
            let matches = unsafe {
                row.data::<String>("memo-id")
                    .is_some_and(|value| value.as_ref() == &identity.uid)
                    && row
                        .data::<String>("memo-source-uid")
                        .is_some_and(|value| value.as_ref() == &identity.source_uid)
            };
            if matches {
                memo_list.select_row(Some(&row));
                break;
            }
        }
    }
    if let Some(focus) = focus {
        let mut child = memo_list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(row) = widget.downcast::<gtk::ListBoxRow>() else {
                continue;
            };
            if !row.is_visible() {
                continue;
            }
            let matches = unsafe {
                row.data::<String>("memo-id")
                    .is_some_and(|value| value.as_ref() == &focus.uid)
                    && row
                        .data::<String>("memo-source-uid")
                        .is_some_and(|value| value.as_ref() == &focus.source_uid)
            };
            if matches {
                memo_list.select_row(Some(&row));
                break;
            }
        }
    }
}

fn refresh_memo_preview(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) {
    let Some(label) = page.imp().memo_preview_label.borrow().as_ref().cloned() else {
        return;
    };
    let state = page.imp().memo_preview_state.borrow().clone();
    let identities = memo_selection_identities(memo_list);
    if !state.visible || identities.len() != 1 {
        label.set_markup("<i>Keine eindeutige Memo-Auswahl</i>");
        return;
    }
    let selected = memo_records.borrow();
    match preview_for_selection(&selected, &identities) {
        Ok(Some(preview)) => label.set_markup(&preview.markup),
        Ok(None) => label.set_markup("<i>Keine eindeutige Memo-Auswahl</i>"),
        Err(error) => label.set_markup(&format!(
            "<i>Memo-Vorschau nicht verfügbar: {}</i>",
            glib::markup_escape_text(&error)
        )),
    }
}

fn update_memo_surface_widgets(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    save_button: &gtk::Button,
    export_button: &gtk::Button,
    delete_button: &gtk::Button,
    open_button: &gtk::Button,
    forward_button: &gtk::Button,
    open_url_button: &gtk::Button,
    print_button: &gtk::Button,
    save_as_button: &gtk::Button,
    copy_button: &gtk::Button,
    cut_button: &gtk::Button,
    paste_button: &gtk::Button,
    select_all_button: &gtk::Button,
) {
    let selected = selected_memo_records_checked(memo_list, memo_records).unwrap_or_default();
    let action_state = MemoActionState::from_selection(&selected, |source_uid| {
        memo_source_is_writable_for_page(page, source_uid)
    });
    save_button
        .set_sensitive(action_state.single_selection && action_state.all_selected_sources_writable);
    export_button.set_sensitive(action_state.can_export);
    delete_button.set_sensitive(action_state.can_delete);
    open_button.set_sensitive(action_state.can_open);
    forward_button.set_sensitive(action_state.can_forward);
    open_url_button.set_sensitive(action_state.can_open_url);
    print_button.set_sensitive(action_state.can_print);
    save_as_button.set_sensitive(action_state.can_save_as);
    copy_button.set_sensitive(action_state.can_copy);
    cut_button.set_sensitive(action_state.can_cut);
    let destination = page
        .imp()
        .memo_source_filter
        .borrow()
        .clone()
        .unwrap_or_else(|| "local-calendar".to_string());
    paste_button.set_sensitive(memo_source_is_writable_for_page(page, &destination));
    select_all_button
        .set_sensitive(action_state.can_select_all && memo_list.first_child().is_some());
    refresh_memo_preview(page, memo_list, memo_records);
}

fn update_memo_surface_widgets_from_page(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
) {
    let Some(buttons) = page.imp().memo_surface_buttons.borrow().as_ref().cloned() else {
        return;
    };
    if buttons.len() != 12 {
        return;
    }
    update_memo_surface_widgets(
        page,
        memo_list,
        memo_records,
        &buttons[0],
        &buttons[1],
        &buttons[2],
        &buttons[3],
        &buttons[4],
        &buttons[5],
        &buttons[6],
        &buttons[7],
        &buttons[8],
        &buttons[9],
        &buttons[10],
        &buttons[11],
    );
}

fn memo_record_from_component(
    source_uid: &str,
    source_display_name: &str,
    component: &str,
    expected_uid: Option<&str>,
) -> Result<MemoRecord, String> {
    let draft = MemoEditorDraft::parse(component)?;
    if draft.uid.trim().is_empty() {
        return Err("Bestätigte VJOURNAL-Komponente hat keine UID".to_string());
    }
    if expected_uid.is_some_and(|expected_uid| draft.uid != expected_uid) {
        return Err("Bestätigte VJOURNAL-Komponente hat nicht die angeforderte UID".to_string());
    }
    Ok(MemoRecord {
        uid: draft.uid,
        source_uid: source_uid.to_string(),
        source_display_name: source_display_name.to_string(),
        component: component.to_string(),
        summary: draft.summary,
        description: draft.description,
        updated: Utc::now().format("%Y-%m-%d %H:%M").to_string(),
    })
}

/// Execute a paste batch off the GTK thread.  Every create is confirmed by
/// the backend's UID and component before it becomes a model record.  If a
/// later item fails, already-created items are removed again so the UI never
/// presents a partial local success.
fn execute_memo_paste_batch(
    plan: Vec<MemoPasteItem>,
) -> Result<Vec<(MemoPasteItem, MemoRecord)>, String> {
    let service = ECalService::new();
    let mut created = Vec::<(String, String)>::new();
    let mut confirmed = Vec::with_capacity(plan.len());
    let mut failure = None;

    for item in plan {
        let source_uid = item.identity.source_uid.clone();
        let expected_uid = item.identity.uid.clone();
        match service.create_component(&source_uid, ECalComponentType::Memo, &item.component) {
            Ok(mutation) => {
                if !mutation.uid.trim().is_empty() {
                    created.push((source_uid.clone(), mutation.uid.clone()));
                }
                if mutation.uid != expected_uid {
                    failure = Some(format!(
                        "{} / {}: Backend-UID {} stimmt nicht überein",
                        source_uid, expected_uid, mutation.uid
                    ));
                    break;
                }
                let Some(component) = mutation.component else {
                    failure = Some(format!(
                        "{} / {}: Backend lieferte keine bestätigte VJOURNAL-Komponente",
                        source_uid, expected_uid
                    ));
                    break;
                };
                match memo_record_from_component(
                    &source_uid,
                    "Zielquelle",
                    &component,
                    Some(&expected_uid),
                ) {
                    Ok(memo) => confirmed.push((item, memo)),
                    Err(error) => {
                        failure = Some(format!("{} / {}: {error}", source_uid, expected_uid));
                        break;
                    }
                }
            }
            Err(error) => {
                failure = Some(format!("{} / {}: {error}", source_uid, expected_uid));
                break;
            }
        }
    }

    let Some(failure) = failure else {
        return Ok(confirmed);
    };
    let rollback_errors = rollback_created_memos(&service, &created);
    if rollback_errors.is_empty() {
        Err(format!("Einfügen atomar abgebrochen: {failure}"))
    } else {
        Err(format!(
            "Einfügen fehlgeschlagen; Rollback unvollständig: {failure}; {}",
            rollback_errors.join("; ")
        ))
    }
}

fn rollback_created_memos(service: &ECalService, created: &[(String, String)]) -> Vec<String> {
    created
        .iter()
        .rev()
        .filter_map(|(source_uid, uid)| {
            match service.remove_component(source_uid, ECalComponentType::Memo, uid) {
                Ok(mutation) if mutation.uid == *uid => None,
                Ok(mutation) => Some(format!(
                    "Rollback {} / {} lieferte UID {}",
                    source_uid, uid, mutation.uid
                )),
                Err(error) => Some(format!("Rollback {} / {}: {error}", source_uid, uid)),
            }
        })
        .collect()
}

/// Delete a batch off the GTK thread and restore already-deleted components
/// when a later UID fails.  The returned identities are consumed only after
/// the whole operation has succeeded.
fn execute_memo_delete_batch(planned: Vec<MemoRecord>) -> Result<Vec<MemoIdentity>, String> {
    let service = ECalService::new();
    let mut deleted = Vec::with_capacity(planned.len());
    let mut failure = None;

    for memo in planned {
        let identity = MemoIdentity::from(&memo);
        match service.remove_component(&memo.source_uid, ECalComponentType::Memo, &memo.uid) {
            Ok(mutation) if mutation.uid == memo.uid => deleted.push(memo),
            Ok(mutation) => {
                failure = Some(format!(
                    "{} / {}: Backend-UID {} stimmt nicht mit {} überein",
                    identity.source_uid, identity.uid, mutation.uid, identity.uid
                ));
                break;
            }
            Err(error) => {
                failure = Some(format!(
                    "{} / {}: {error}",
                    identity.source_uid, identity.uid
                ));
                break;
            }
        }
    }

    let Some(failure) = failure else {
        return Ok(deleted.iter().map(MemoIdentity::from).collect());
    };

    let mut rollback_errors = Vec::new();
    for memo in deleted.iter().rev() {
        match service.create_component(&memo.source_uid, ECalComponentType::Memo, &memo.component) {
            Ok(mutation) if mutation.uid == memo.uid => {}
            Ok(mutation) => rollback_errors.push(format!(
                "Rollback {} / {} lieferte UID {}",
                memo.source_uid, memo.uid, mutation.uid
            )),
            Err(error) => rollback_errors.push(format!(
                "Rollback {} / {}: {error}",
                memo.source_uid, memo.uid
            )),
        }
    }
    if rollback_errors.is_empty() {
        Err(format!("Löschen atomar abgebrochen: {failure}"))
    } else {
        Err(format!(
            "Löschen fehlgeschlagen; Rollback unvollständig: {failure}; {}",
            rollback_errors.join("; ")
        ))
    }
}

fn reload_memo_snapshot_async(
    page: &MemosPage,
    memo_list: &gtk::ListBox,
    memo_records: &Rc<RefCell<Vec<MemoRecord>>>,
    empty_state: &gtk::Label,
) {
    let generation = page.imp().memo_refresh_generation.get().wrapping_add(1);
    page.imp().memo_refresh_generation.set(generation);
    let page_weak = page.downgrade();
    let memo_list = memo_list.clone();
    let memo_records = Rc::clone(memo_records);
    let empty_state = empty_state.clone();
    let (sender, receiver) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = load_snapshot_prefer_eds();
        if let Err(error) = sender.send_blocking(result) {
            tracing::debug!(%error, "Memo recovery snapshot was dropped");
        }
    });
    glib::spawn_future_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let Some(page) = page_weak.upgrade() else {
            return;
        };
        if page.imp().memo_refresh_generation.get() != generation {
            return;
        }
        if let Some(warning) = result.warning {
            tracing::warn!(%warning, "Memo recovery snapshot loaded with a backend warning");
        }
        let memo_count = result.snapshot.memos.len();
        replace_memo_snapshot(&memo_list, Rc::clone(&memo_records), result.snapshot.memos);
        empty_state.set_visible(memo_count == 0);
        empty_state.set_label(if memo_count == 0 {
            "Keine synchronisierten Notizen gefunden"
        } else {
            ""
        });
        refresh_memo_category_model(&page, &memo_list);
        refresh_memo_filter_view(&page, &memo_list, &memo_records, &empty_state);
    });
}

fn memo_source_is_writable_for_page(page: &MemosPage, source_uid: &str) -> bool {
    if source_uid.trim().is_empty() {
        return false;
    }
    if source_uid == "local-calendar" {
        return CalendarStoreService::new().is_writable();
    }
    if page
        .imp()
        .memo_unavailable_sources
        .borrow()
        .contains(source_uid)
    {
        return false;
    }
    // Remote mutating operations require the authoritative client read-only
    // capability.  An unknown, unavailable, or read-only source never falls
    // back to a guessed writable state.
    page.imp()
        .memo_available_sources
        .borrow()
        .contains(source_uid)
        && page
            .imp()
            .memo_writable_sources
            .borrow()
            .contains(source_uid)
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
    let selection = memo_selection_from_rows(memo_list);
    let Some(index) = memo_records
        .borrow()
        .iter()
        .position(|memo| memo.source_uid == source_uid && memo.uid == uid)
    else {
        return false;
    };
    memo_records.borrow_mut().remove(index);
    remove_memo_list_row(memo_list, source_uid, uid);
    restore_memo_selection_rows(memo_list, &selection);
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

fn save_memo_file_async(
    button: &gtk::Button,
    file: gio::File,
    payload: String,
    success_message: &'static str,
) {
    let button = button.clone();
    file.replace_contents_async(
        payload.into_bytes(),
        None,
        true,
        gio::FileCreateFlags::NONE,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(_) => show_toast_for_widget(&button, success_message),
            Err((_, error)) => show_toast_for_widget(
                &button,
                &format!("iCalendar konnte nicht gespeichert werden: {error}"),
            ),
        },
    );
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
    fn memo_source_capability_projection_is_exact_sorted_and_fail_closed() {
        let sources = project_memo_source_capabilities(vec![
            ECalSourceCapability {
                source_uid: "readonly".to_string(),
                display_name: "Zulu".to_string(),
                connected: true,
                available: true,
                writable: false,
                source_writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "offline".to_string(),
                display_name: "Alpha".to_string(),
                connected: false,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "writable".to_string(),
                display_name: "Beta".to_string(),
                connected: true,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "writable".to_string(),
                display_name: "Duplicate must not win".to_string(),
                connected: true,
                available: true,
                writable: false,
                source_writable: true,
                refresh_supported: false,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
            ECalSourceCapability {
                source_uid: "  ".to_string(),
                display_name: "Invalid".to_string(),
                connected: true,
                available: true,
                writable: true,
                source_writable: true,
                refresh_supported: true,
                removable: false,
                remote_deletable: false,
                in_collection: false,
                task_assignment_supported: false,
                task_assignment_conversion_supported: false,
            },
        ]);

        assert_eq!(
            sources
                .iter()
                .map(|source| source.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["offline", "writable", "readonly"]
        );
        assert!(!sources[0].available);
        assert!(!sources[0].writable);
        assert_eq!(
            memo_source_projection_label(&sources[0]),
            "Nicht erreichbar · Alpha"
        );
        assert!(sources[1].available);
        assert!(sources[1].writable);
        assert!(!sources[2].writable);
        assert_eq!(
            memo_source_projection_label(&sources[2]),
            "Schreibgeschützt · Zulu"
        );
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
        assert!(memo_record_from_component("source", "Source", component, Some("other")).is_err());
        assert!(
            memo_record_from_component("source", "Source", component, Some("confirmed")).is_ok()
        );
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
