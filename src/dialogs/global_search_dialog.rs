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

use crate::services::search_engine::{GlobalSearchEngine, SearchDomain, SearchResultItem};

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
        type ParentType = adw::Window;
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
    impl WindowImpl for GlobalSearchDialog {}
    impl AdwWindowImpl for GlobalSearchDialog {}
}

glib::wrapper! {
    pub struct GlobalSearchDialog(ObjectSubclass<imp::GlobalSearchDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl GlobalSearchDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Globale Suche")
            .property("modal", false)
            .property("default-width", 760)
            .property("default-height", 600)
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
        self.set_content(Some(&toolbar_view));

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
            dialog.imp().debounce_source.borrow_mut().take();
            dialog.start_query(query, domain, generation);
        });
        self.imp().debounce_source.replace(Some(debounce));
    }

    fn start_query(&self, query: String, domain: SearchDomain, generation: u64) {
        if generation != self.imp().query_generation.get() {
            return;
        }

        let cancellation = Arc::new(AtomicBool::new(false));
        self.imp()
            .query_cancellation
            .replace(Some(cancellation.clone()));
        let (sender, receiver) = async_channel::bounded(1);
        let query_for_worker = query.clone();
        std::thread::spawn(move || {
            if cancellation.load(Ordering::Relaxed) {
                return;
            }
            let results =
                GlobalSearchEngine::query_cancelable(domain, &query_for_worker, &cancellation);
            if let Err(error) = sender.send_blocking(results) {
                tracing::debug!(%error, "Global search result was dropped");
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
            if dialog.imp().query_generation.get() != generation {
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
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(&subtitle)
            .activatable(result.domain != SearchDomain::All)
            .selectable(false)
            .build();
        if result.domain != SearchDomain::All {
            let domain = result.domain;
            let reference_id = result.reference_id.clone();
            let result_title = result.title.clone();
            row.connect_activated(glib::clone!(
                #[weak(rename_to = dialog)]
                self,
                move |_| {
                    let Some(parent) = dialog.transient_for().and_then(|window| {
                        window.downcast::<crate::window::EvolutionWindow>().ok()
                    }) else {
                        return;
                    };
                    parent.show_search_result(domain, reference_id.clone(), result_title.clone());
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

impl Default for GlobalSearchDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_selector_has_stable_backend_mapping() {
        assert_eq!(search_domain_for_index(0), SearchDomain::All);
        assert_eq!(search_domain_for_index(1), SearchDomain::Mail);
        assert_eq!(search_domain_for_index(2), SearchDomain::Calendar);
        assert_eq!(search_domain_for_index(3), SearchDomain::Contacts);
        assert_eq!(search_domain_for_index(99), SearchDomain::All);
    }
}
