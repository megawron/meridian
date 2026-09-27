// SPDX-License-Identifier: GPL-3.0-or-later
//
// GlobalSearchDialog — portable cross-component search.
//
// The dialog intentionally consumes the backend-neutral search service. It
// can therefore be used on every development platform and does not pretend
// that a local scan is equivalent to CamelFolderSearch or an EDS live query.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::services::search_engine::{
    GlobalSearchEngine, SearchDomain, SearchResultItem, SearchStoreSnapshot,
};

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct GlobalSearchDialog {
        pub list: RefCell<Option<gtk::ListBox>>,
        pub status: RefCell<Option<gtk::Label>>,
        pub domain: RefCell<SearchDomain>,
        pub query_generation: Cell<u64>,
        pub query_cancellation: RefCell<Option<Arc<AtomicBool>>>,
        pub debounce_source: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GlobalSearchDialog {
        const NAME: &'static str = "EvolutionGlobalSearchDialog";
        type Type = super::GlobalSearchDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for GlobalSearchDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }

        fn dispose(&self) {
            if let Some(source) = self.debounce_source.borrow_mut().take() {
                source.remove();
            }
            if let Some(cancellation) = self.query_cancellation.borrow_mut().take() {
                cancellation.store(true, Ordering::Relaxed);
            }
        }
    }

    impl WidgetImpl for GlobalSearchDialog {}
    impl AdwDialogImpl for GlobalSearchDialog {}
}

glib::wrapper! {
    pub struct GlobalSearchDialog(ObjectSubclass<imp::GlobalSearchDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl GlobalSearchDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Globale Suche")
            .property("content-width", 760)
            .property("content-height", 600)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(
                "Mail, Kalender, Aufgaben, Notizen oder Kontakte durchsuchen (on:2026-08-11, after:7d)…",
            )
            .hexpand(true)
            .build();
        let domain_selector = gtk::DropDown::new(
            Some(gtk::StringList::new(&[
                "Alle",
                "Mail",
                "Kalender / Aufgaben / Notizen",
                "Kontakte",
            ])),
            None::<&gtk::Expression>,
        );
        domain_selector.set_selected(0);
        domain_selector.set_tooltip_text(Some("Suchbereich auswählen"));
        let header_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .build();
        header_content.append(&search_entry);
        header_content.append(&domain_selector);
        header_bar.set_title_widget(Some(&header_content));
        toolbar_view.add_top_bar(&header_bar);

        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .wrap(true)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(8)
            .css_classes(vec!["dim-label"])
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(16)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&status);
        content.append(&scrolled);
        toolbar_view.set_content(Some(&content));
        self.set_child(Some(&toolbar_view));

        self.imp().list.replace(Some(list));
        self.imp().status.replace(Some(status));
        self.imp().domain.replace(SearchDomain::All);

        search_entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |entry| dialog.reload(entry.text().as_str())
        ));
        let search_entry_for_domain = search_entry.clone();
        domain_selector.connect_selected_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |selector| {
                dialog
                    .imp()
                    .domain
                    .replace(search_domain_for_index(selector.selected()));
                dialog.reload(search_entry_for_domain.text().as_str());
            }
        ));
        search_entry.grab_focus();
        self.set_status(
            "Portable Suche über lokale Mail-, Kalender-, Aufgaben-, Notizen- und Kontaktdaten. \
             Serverseitige Kontosuche wird auf Linux separat aktiviert.",
        );
    }

    fn reload(&self, query: &str) {
        let Some(list) = self.imp().list.borrow().as_ref().cloned() else {
            return;
        };
        if let Some(source) = self.imp().debounce_source.borrow_mut().take() {
            source.remove();
        }
        if let Some(cancellation) = self.imp().query_cancellation.borrow_mut().take() {
            cancellation.store(true, Ordering::Relaxed);
        }
        let generation = self.imp().query_generation.get().wrapping_add(1);
        self.imp().query_generation.set(generation);
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let query = query.trim();
        if query.is_empty() {
            self.set_status(
                "Suchbegriff eingeben. Durchsucht werden die app-eigenen lokalen Datenbestände.",
            );
            return;
        }

        let domain = *self.imp().domain.borrow();
        self.set_status("Lokale Datenbestände werden durchsucht …");
        let query = query.to_string();
        let dialog = self.downgrade();
        let debounce = glib::timeout_add_local_once(Duration::from_millis(120), move || {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            if !generation_is_current(dialog.imp().query_generation.get(), generation) {
                return;
            }
            dialog.imp().debounce_source.borrow_mut().take();
            dialog.start_query(query, domain, generation);
        });
        self.imp().debounce_source.replace(Some(debounce));
    }

    fn start_query(&self, query: String, domain: SearchDomain, generation: u64) {
        if !generation_is_current(self.imp().query_generation.get(), generation) {
            return;
        }

        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .query_cancellation
            .replace(Some(cancellation.clone()));
        let (sender, receiver) = async_channel::bounded(1);
        let stores = SearchStoreSnapshot::capture_on_main_thread();
        let query_for_worker = query.clone();
        let cancellation_for_result = cancellation.clone();
        std::thread::spawn(move || {
            if cancellation.load(Ordering::Relaxed) {
                return;
            }
            let results = GlobalSearchEngine::query_cancelable_with_stores(
                domain,
                &query_for_worker,
                &cancellation,
                stores,
            );
            if !cancellation.load(Ordering::Relaxed) {
                if let Err(error) = sender.send_blocking(results) {
                    tracing::debug!(%error, "Global search result was dropped");
                }
            }
        });

        let dialog = self.downgrade();
        glib::spawn_future_local(async move {
            let Ok(results) = receiver.recv().await else {
                return;
            };
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            if !query_result_is_current(
                dialog.imp().query_generation.get(),
                generation,
                &cancellation_for_result,
            ) {
                return;
            }
            dialog.imp().query_cancellation.borrow_mut().take();
            dialog.show_results(&query, results);
        });
    }

    fn show_results(&self, query: &str, results: Vec<SearchResultItem>) {
        let Some(list) = self.imp().list.borrow().as_ref().cloned() else {
            return;
        };
        if results.is_empty() {
            self.set_status(&format!("Keine lokalen Treffer für „{query}“."));
            return;
        }

        self.set_status(&format!(
            "{} lokale Treffer für „{query}“ — die Kontosuche bleibt auf Linux separat.",
            results.len()
        ));
        for result in results {
            list.append(&self.result_row(&result));
        }
    }

    fn result_row(&self, result: &SearchResultItem) -> adw::ActionRow {
        let title = if result.title.trim().is_empty() {
            "(Ohne Titel)"
        } else {
            result.title.trim()
        };
        let mut subtitle = match result.domain {
            SearchDomain::Mail => "Mail".to_string(),
            SearchDomain::Contacts => "Kontakt".to_string(),
            SearchDomain::Calendar => "Kalender / Aufgabe / Notiz".to_string(),
            SearchDomain::All => "Lokaler Treffer".to_string(),
        };
        if !result.date.trim().is_empty() {
            subtitle.push_str(" · ");
            subtitle.push_str(result.date.trim());
        }
        let snippet = compact_snippet(&result.snippet);
        if !snippet.is_empty() {
            subtitle.push_str(" · ");
            subtitle.push_str(&snippet);
        }
        let target = search_result_activation_target(result);
        if target.is_none() {
            subtitle.push_str(" · Nicht verfügbar");
        }
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(&subtitle)
            .activatable(target.is_some())
            .selectable(false)
            .build();
        if let Some(target) = target {
            row.connect_activated(glib::clone!(
                #[weak(rename_to = dialog)]
                self,
                move |_| {
                    let parent = dialog
                        .root()
                        .and_downcast::<crate::window::EvolutionWindow>()
                        .or_else(|| {
                            gio::Application::default()
                                .and_then(|app| app.downcast::<adw::Application>().ok())
                                .and_then(|app| app.active_window())
                                .and_then(|win| win.downcast::<crate::window::EvolutionWindow>().ok())
                        });
                    let Some(parent) = parent else {
                        return;
                    };
                    parent.show_search_result(Some(target.clone()));
                    dialog.close();
                }
            ));
        }
        row
    }

    fn set_status(&self, message: &str) {
        if let Some(status) = self.imp().status.borrow().as_ref() {
            status.set_text(message);
        }
    }
}

fn search_domain_for_index(index: u32) -> SearchDomain {
    match index {
        1 => SearchDomain::Mail,
        2 => SearchDomain::Calendar,
        3 => SearchDomain::Contacts,
        _ => SearchDomain::All,
    }
}

fn compact_snippet(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut result = value.chars().take(180).collect::<String>();
    if value.chars().count() > 180 {
        result.push('…');
    }
    result
}

fn search_result_activation_target(
    result: &SearchResultItem,
) -> Option<crate::services::search_engine::SearchTarget> {
    result.target.clone()
}

fn generation_is_current(current_generation: u64, candidate_generation: u64) -> bool {
    current_generation == candidate_generation
}

fn query_result_is_current(
    current_generation: u64,
    result_generation: u64,
    cancellation: &AtomicBool,
) -> bool {
    generation_is_current(current_generation, result_generation)
        && !cancellation.load(Ordering::Relaxed)
}

impl Default for GlobalSearchDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_instance_signal_handler(row: &adw::ActionRow, signal_name: &str) -> bool {
        let signal_id = glib::subclass::SignalId::lookup(signal_name, row.type_())
            .expect("ActionRow signal must be registered");
        glib::signal::signal_has_handler_pending(row, signal_id, None, true)
    }

    #[test]
    fn domain_selector_has_stable_backend_mapping() {
        assert_eq!(search_domain_for_index(0), SearchDomain::All);
        assert_eq!(search_domain_for_index(1), SearchDomain::Mail);
        assert_eq!(search_domain_for_index(2), SearchDomain::Calendar);
        assert_eq!(search_domain_for_index(3), SearchDomain::Contacts);
        assert_eq!(search_domain_for_index(99), SearchDomain::All);
    }

    #[test]
    fn stale_or_cancelled_query_results_are_rejected() {
        let cancellation = AtomicBool::new(false);
        assert!(generation_is_current(7, 7));
        assert!(!generation_is_current(8, 7));
        assert!(query_result_is_current(7, 7, &cancellation));
        assert!(!query_result_is_current(8, 7, &cancellation));
        assert!(!query_result_is_current(7, 8, &cancellation));

        cancellation.store(true, Ordering::Relaxed);
        assert!(!query_result_is_current(7, 7, &cancellation));
    }

    #[test]
    fn worker_receives_store_snapshot_before_spawn() {
        let source = include_str!("global_search_dialog.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source should precede tests");
        let capture = production
            .find("SearchStoreSnapshot::capture_on_main_thread()")
            .expect("the main thread must capture search stores");
        let spawn = production
            .find("std::thread::spawn(move ||")
            .expect("the search worker must be spawned");
        assert!(capture < spawn);
        assert!(production.contains("query_cancelable_with_stores("));

        let worker = &production[spawn..];
        assert!(!worker.contains("MailStoreService::new("));
        assert!(!worker.contains("CalendarStoreService::new("));
        assert!(!worker.contains("ContactStoreService::new("));
        assert!(!worker.contains("AppSettings"));
        assert!(!worker.contains("GSettings"));
        assert!(!worker.contains("std::env::"));
        assert!(!worker.contains("app_data_dir("));

        let debounce_generation_check = production
            .find("generation_is_current(dialog.imp().query_generation.get(), generation)")
            .expect("debounce must reject stale generations before starting a query");
        let debounce_source_take = production
            .find("dialog.imp().debounce_source.borrow_mut().take()")
            .expect("debounce source should be cleared when its generation runs");
        assert!(debounce_generation_check < debounce_source_take);
    }

    #[test]
    fn result_rows_forward_typed_targets_without_using_titles_as_identity() {
        let source = include_str!("global_search_dialog.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source should precede tests");
        assert!(production.contains("let target = search_result_activation_target(result);"));
        assert!(production.contains(".activatable(target.is_some())"));
        assert!(production.contains("if let Some(target) = target"));
        assert!(production.contains("parent.show_search_result(Some(target.clone()))"));
    }

    #[test]
    fn unavailable_result_has_no_activation_target() {
        let unavailable = SearchResultItem {
            domain: SearchDomain::All,
            title: "Unavailable".to_string(),
            snippet: String::new(),
            date: String::new(),
            target: None,
        };
        let available = SearchResultItem {
            domain: SearchDomain::Mail,
            title: "Mail".to_string(),
            snippet: String::new(),
            date: String::new(),
            target: Some(crate::services::search_engine::SearchTarget::Mail {
                local_id: "message-1".to_string(),
            }),
        };

        assert!(search_result_activation_target(&unavailable).is_none());
        assert!(search_result_activation_target(&available).is_some());
    }

    #[test]
    fn unavailable_result_row_is_non_activatable_without_activation_handler() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };
        let dialog = GlobalSearchDialog::new();
        let unavailable = SearchResultItem {
            domain: SearchDomain::All,
            title: "Unverfügbarer Treffer".to_string(),
            snippet: "Keine Zielidentität".to_string(),
            date: String::new(),
            target: None,
        };

        let row = dialog.result_row(&unavailable);
        assert_eq!(row.title().as_str(), "Unverfügbarer Treffer");
        assert!(
            row.subtitle()
                .is_some_and(|subtitle| subtitle.contains("Nicht verfügbar"))
        );
        assert!(!row.is_activatable());
        assert!(!has_instance_signal_handler(&row, "activated"));

        // Calling the widget's activation entry point must remain a no-op:
        // there is no dialog callback to fire for a result without a target.
        adw::prelude::ActionRowExt::activate(&row);
        assert!(!has_instance_signal_handler(&row, "activated"));
    }
}
