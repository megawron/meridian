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
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::calendar_store_service::{CalendarStoreService, MemoRecord};

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

        let memo_title = gtk::Label::builder()
            .label("Keine Notiz ausgewählt")
            .css_classes(vec!["title-1"])
            .halign(gtk::Align::Start)
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

        content_box.append(&memo_title);
        content_box.append(&memo_meta);
        content_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content_box.append(&memo_text);

        let memo_records = Rc::new(RefCell::new(Vec::<MemoRecord>::new()));
        let memo_records_for_selection = Rc::clone(&memo_records);
        memo_list.connect_row_selected(glib::clone!(
            #[weak]
            memo_title,
            #[weak]
            memo_meta,
            #[weak]
            memo_text,
            move |_, row| {
                let Some(row) = row else {
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
                memo_title.set_label(&memo.summary);
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
            let snapshot = CalendarStoreService::new().load_snapshot();
            if let Err(error) = memo_sender.send_blocking(snapshot) {
                tracing::debug!(%error, "Memo snapshot was dropped before delivery");
            }
        });
        glib::spawn_future_local(async move {
            match memo_receiver.recv().await {
                Ok(snapshot) => {
                    if snapshot.memos.is_empty() {
                        empty_state_for_load.set_label("Keine lokalen VJOURNAL-Daten gefunden");
                    } else {
                        empty_state_for_load.set_visible(false);
                        memo_meta_for_load.set_label(&format!(
                            "{} lokale Notiz(en) aus iCalendar geladen",
                            snapshot.memos.len()
                        ));
                        let mut records = memo_records_for_load.borrow_mut();
                        for memo in snapshot.memos {
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
}

impl Default for MemosPage {
    fn default() -> Self {
        Self::new()
    }
}
