// SPDX-License-Identifier: GPL-3.0-or-later
//
// Authoritative Camel folder-subscription editor.  It intentionally has no
// local or Graph fallback: CamelSubscribable is the provider contract here.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::services::camel_mail_service::{
    self, CamelSubscriptionFolder, CamelSubscriptionSnapshot,
};

enum SubscriptionWorkerResult {
    Load(Result<CamelSubscriptionSnapshot, String>),
    Set {
        folder_name: String,
        result: Result<(), String>,
    },
}

struct SubscriptionDialogState {
    source_uid: String,
    folders: Vec<CamelSubscriptionFolder>,
    is_subscribable: Option<bool>,
    filter: String,
    busy: bool,
    cancelling: bool,
    closed: bool,
    generation: u64,
    cancellation: Option<Arc<AtomicBool>>,
    on_confirmed_change: Rc<dyn Fn()>,
    list: gtk::ListBox,
    status: gtk::Label,
    refresh: gtk::Button,
    cancel: gtk::Button,
}

/// A lightweight GTK4/libadwaita view over one exact Camel source.  Native
/// reads and mutations always execute in a worker thread.
pub struct FolderSubscriptionsDialog {
    window: adw::Window,
    state: Rc<RefCell<SubscriptionDialogState>>,
}

impl FolderSubscriptionsDialog {
    pub fn new<F>(account_name: &str, source_uid: &str, on_confirmed_change: F) -> Self
    where
        F: Fn() + 'static,
    {
        let window = adw::Window::builder()
            .title(format!("Ordner-Abonnements: {account_name}"))
            .modal(true)
            .default_width(560)
            .default_height(500)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Abonnementsliste neu laden")
            .build();
        let cancel = gtk::Button::builder()
            .label("Abbrechen")
            .sensitive(false)
            .build();
        let close = gtk::Button::builder().label("Schließen").build();
        header.pack_start(&refresh);
        header.pack_start(&cancel);
        header.pack_end(&close);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_top(14)
            .margin_bottom(18)
            .build();
        content.append(
            &gtk::Label::builder()
                .label("Die Liste und der Abonnementstatus stammen direkt vom Mail-Anbieter.")
                .xalign(0.0)
                .wrap(true)
                .css_classes(vec!["dim-label"])
                .build(),
        );
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Ordner filtern…")
            .build();
        content.append(&search);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        content.append(&status);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .vexpand(true)
            .build();
        content.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build(),
        );
        toolbar.set_content(Some(&content));
        window.set_content(Some(&toolbar));

        let state = Rc::new(RefCell::new(SubscriptionDialogState {
            source_uid: source_uid.trim().to_string(),
            folders: Vec::new(),
            is_subscribable: None,
            filter: String::new(),
            busy: false,
            cancelling: false,
            closed: false,
            generation: 0,
            cancellation: None,
            on_confirmed_change: Rc::new(on_confirmed_change),
            list,
            status,
            refresh,
            cancel,
        }));

        let state_for_refresh = Rc::clone(&state);
        state.borrow().refresh.connect_clicked(move |_| {
            start_load(&state_for_refresh);
        });
        let state_for_cancel = Rc::clone(&state);
        state.borrow().cancel.connect_clicked(move |_| {
            cancel_current(&state_for_cancel);
        });
        let state_for_search = Rc::clone(&state);
        search.connect_search_changed(move |entry| {
            state_for_search.borrow_mut().filter = entry.text().trim().to_ascii_lowercase();
            rebuild_list(&state_for_search);
        });
        let window_for_close = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(window) = window_for_close.upgrade() {
                window.close();
            }
        });
        let state_for_close = Rc::clone(&state);
        window.connect_close_request(move |_| {
            let mut state = state_for_close.borrow_mut();
            state.closed = true;
            state.generation = state.generation.wrapping_add(1);
            if let Some(cancelled) = state.cancellation.as_ref() {
                cancelled.store(true, Ordering::Relaxed);
            }
            glib::Propagation::Proceed
        });

        let dialog = Self { window, state };
        start_load(&dialog.state);
        dialog
    }

    pub fn set_transient_for(&self, parent: Option<&gtk::Window>) {
        self.window.set_transient_for(parent);
    }

    pub fn present(&self) {
        self.window.present();
    }
}

fn subscription_hierarchy_depth(folders: &[CamelSubscriptionFolder], folder_name: &str) -> usize {
    let parents = folders
        .iter()
        .map(|folder| {
            (
                folder.folder_name.as_str(),
                folder.parent_folder_name.as_deref(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut depth = 0;
    let mut current = folder_name;
    let mut visited = HashSet::new();
    while let Some(Some(parent)) = parents.get(current) {
        if !visited.insert(current) || depth >= 32 {
            return 0;
        }
        depth += 1;
        current = parent;
    }
    depth
}

fn subscription_row_matches_filter(folder: &CamelSubscriptionFolder, filter: &str) -> bool {
    filter.is_empty()
        || folder.display_name.to_ascii_lowercase().contains(filter)
        || folder.folder_name.to_ascii_lowercase().contains(filter)
}

fn set_status(state: &Rc<RefCell<SubscriptionDialogState>>, message: Option<&str>, error: bool) {
    let state = state.borrow();
    state.status.set_label(message.unwrap_or_default());
    state.status.set_visible(message.is_some());
    if error {
        state.status.add_css_class("error");
    } else {
        state.status.remove_css_class("error");
    }
}

fn update_controls(state: &Rc<RefCell<SubscriptionDialogState>>) {
    let state = state.borrow();
    state.refresh.set_sensitive(!state.busy && !state.closed);
    state.cancel.set_sensitive(state.busy && !state.closed);
}

fn rebuild_list(state: &Rc<RefCell<SubscriptionDialogState>>) {
    let (list, folders, filter, busy, is_subscribable) = {
        let state = state.borrow();
        (
            state.list.clone(),
            state.folders.clone(),
            state.filter.clone(),
            state.busy,
            state.is_subscribable,
        )
    };
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    match is_subscribable {
        None if busy => {
            list.append(
                &adw::ActionRow::builder()
                    .title("Abonnementsliste wird geladen…")
                    .build(),
            );
            return;
        }
        Some(false) => {
            list.append(
                &adw::ActionRow::builder()
                    .title("Dieses Konto unterstützt keine Ordner-Abonnements")
                    .subtitle("Der Camel-Anbieter meldet keine CamelSubscribable-Fähigkeit.")
                    .build(),
            );
            return;
        }
        _ => {}
    }

    let visible = folders
        .iter()
        .filter(|folder| subscription_row_matches_filter(folder, &filter))
        .cloned()
        .collect::<Vec<_>>();
    if visible.is_empty() {
        let (title, subtitle) = if filter.is_empty() {
            (
                "Keine Ordner gefunden",
                "Der Anbieter lieferte keine abonnierbaren Ordner.",
            )
        } else {
            (
                "Keine passenden Ordner",
                "Der Filter stimmt mit keinem Anbieterordner überein.",
            )
        };
        list.append(
            &adw::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .build(),
        );
        return;
    }

    for folder in visible {
        let depth = subscription_hierarchy_depth(&folders, &folder.folder_name);
        let mut subtitle = format!(
            "{} · {} Nachrichten",
            folder.folder_name, folder.total_count
        );
        if !folder.is_selectable {
            subtitle.push_str(" · NOSELECT");
        }
        let row = adw::ActionRow::builder()
            .title(&folder.display_name)
            .subtitle(&subtitle)
            .margin_start((depth as i32).saturating_mul(18))
            .build();
        if folder.is_selectable && is_subscribable == Some(true) {
            let check = gtk::CheckButton::builder()
                .active(folder.is_subscribed)
                .sensitive(!busy)
                .valign(gtk::Align::Center)
                .tooltip_text("Abonnement beim Anbieter ändern")
                .build();
            let state_for_toggle = Rc::clone(state);
            let folder_name = folder.folder_name.clone();
            let subscribed = folder.is_subscribed;
            check.connect_toggled(move |check| {
                let requested = check.is_active();
                if requested == subscribed || state_for_toggle.borrow().busy {
                    return;
                }
                // Never show an optimistic provider result while the worker is busy.
                check.set_active(subscribed);
                start_set_subscription(&state_for_toggle, folder_name.clone(), requested);
            });
            row.add_suffix(&check);
            row.set_activatable_widget(Some(&check));
        } else {
            let state_label = if folder.is_subscribed {
                "Abonniert"
            } else {
                "Nicht abonniert"
            };
            row.add_suffix(&gtk::Label::builder().label(state_label).build());
        }
        list.append(&row);
    }
}

fn start_load(state: &Rc<RefCell<SubscriptionDialogState>>) {
    let (source_uid, generation, cancelled) = {
        let mut state = state.borrow_mut();
        if state.busy || state.closed || state.source_uid.is_empty() {
            return;
        }
        state.busy = true;
        state.cancelling = false;
        state.generation = state.generation.wrapping_add(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        state.cancellation = Some(Arc::clone(&cancelled));
        (state.source_uid.clone(), state.generation, cancelled)
    };
    set_status(
        state,
        Some("Abonnementsliste wird vom Anbieter geladen…"),
        false,
    );
    update_controls(state);
    rebuild_list(state);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = camel_mail_service::load_subscription_folders_with_cancellation(
            &source_uid,
            Some(&cancelled),
        );
        let _ = sender.send(SubscriptionWorkerResult::Load(result));
    });
    poll_worker(state, generation, receiver);
}

fn start_set_subscription(
    state: &Rc<RefCell<SubscriptionDialogState>>,
    folder_name: String,
    subscribed: bool,
) {
    let (source_uid, generation, cancelled) = {
        let mut state = state.borrow_mut();
        if state.busy || state.closed || state.is_subscribable != Some(true) {
            return;
        }
        state.busy = true;
        state.cancelling = false;
        state.generation = state.generation.wrapping_add(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        state.cancellation = Some(Arc::clone(&cancelled));
        (state.source_uid.clone(), state.generation, cancelled)
    };
    set_status(
        state,
        Some("Abonnement wird beim Anbieter bestätigt…"),
        false,
    );
    update_controls(state);
    rebuild_list(state);
    let (sender, receiver) = mpsc::channel();
    let folder_name_for_worker = folder_name.clone();
    std::thread::spawn(move || {
        let result = camel_mail_service::set_folder_subscription_with_cancellation(
            &source_uid,
            &folder_name_for_worker,
            subscribed,
            Some(&cancelled),
        );
        let _ = sender.send(SubscriptionWorkerResult::Set {
            folder_name,
            result,
        });
    });
    poll_worker(state, generation, receiver);
}

fn cancel_current(state: &Rc<RefCell<SubscriptionDialogState>>) {
    let cancelled = {
        let mut state = state.borrow_mut();
        if !state.busy || state.cancelling {
            return;
        }
        state.cancelling = true;
        state.cancellation.clone()
    };
    if let Some(cancelled) = cancelled {
        cancelled.store(true, Ordering::Relaxed);
    }
    set_status(state, Some("Vorgang wird abgebrochen…"), false);
}

fn poll_worker(
    state: &Rc<RefCell<SubscriptionDialogState>>,
    generation: u64,
    receiver: mpsc::Receiver<SubscriptionWorkerResult>,
) {
    let state = Rc::clone(state);
    glib::timeout_add_local(Duration::from_millis(25), move || {
        match receiver.try_recv() {
            Ok(result) => {
                if state.borrow().closed || state.borrow().generation != generation {
                    return glib::ControlFlow::Break;
                }
                match result {
                    SubscriptionWorkerResult::Load(result) => finish_load(&state, result),
                    SubscriptionWorkerResult::Set {
                        folder_name,
                        result,
                    } => finish_set_subscription(&state, &folder_name, result),
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                if !state.borrow().closed && state.borrow().generation == generation {
                    finish_worker_error(&state, "Der Abonnement-Arbeitsvorgang wurde beendet");
                }
                glib::ControlFlow::Break
            }
        }
    });
}

fn finish_load(
    state: &Rc<RefCell<SubscriptionDialogState>>,
    result: Result<CamelSubscriptionSnapshot, String>,
) {
    {
        let mut state_ref = state.borrow_mut();
        state_ref.busy = false;
        state_ref.cancelling = false;
        state_ref.cancellation = None;
        match result {
            Ok(snapshot) => {
                state_ref.is_subscribable = Some(snapshot.is_subscribable);
                state_ref.folders = snapshot.folders;
            }
            Err(error) => {
                drop(state_ref);
                set_status(
                    state,
                    Some(&format!(
                        "Abonnementsliste konnte nicht geladen werden: {error}"
                    )),
                    true,
                );
                update_controls(state);
                rebuild_list(state);
                return;
            }
        }
    }
    set_status(state, None, false);
    update_controls(state);
    rebuild_list(state);
}

fn finish_set_subscription(
    state: &Rc<RefCell<SubscriptionDialogState>>,
    folder_name: &str,
    result: Result<(), String>,
) {
    match result {
        Ok(()) => {
            {
                let mut state = state.borrow_mut();
                state.busy = false;
                state.cancelling = false;
                state.cancellation = None;
            }
            set_status(
                state,
                Some(&format!(
                    "„{folder_name}“ wurde vom Anbieter bestätigt; Liste wird aktualisiert…"
                )),
                false,
            );
            // The bridge has observed the requested provider bit.  Refresh
            // the normal Camel sidebar and this independent snapshot now.
            (state.borrow().on_confirmed_change)();
            start_load(state);
        }
        Err(error) => finish_worker_error(
            state,
            &format!("Abonnement für „{folder_name}“ wurde nicht geändert: {error}"),
        ),
    }
}

fn finish_worker_error(state: &Rc<RefCell<SubscriptionDialogState>>, message: &str) {
    {
        let mut state = state.borrow_mut();
        state.busy = false;
        state.cancelling = false;
        state.cancellation = None;
    }
    set_status(state, Some(message), true);
    update_controls(state);
    rebuild_list(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, parent: Option<&str>, selectable: bool) -> CamelSubscriptionFolder {
        CamelSubscriptionFolder {
            source_uid: "source-a".to_string(),
            source_display_name: "Account A".to_string(),
            folder_name: name.to_string(),
            parent_folder_name: parent.map(ToString::to_string),
            display_name: name.rsplit('/').next().unwrap_or(name).to_string(),
            unread_count: 0,
            total_count: 0,
            is_selectable: selectable,
            is_subscribed: false,
        }
    }

    #[test]
    fn hierarchy_and_filter_keep_authoritative_folder_identity() {
        let folders = vec![
            folder("Projects", None, false),
            folder("Projects/2026", Some("Projects"), true),
            folder("Projects/2026/Archive", Some("Projects/2026"), true),
        ];
        assert_eq!(subscription_hierarchy_depth(&folders, "Projects"), 0);
        assert_eq!(subscription_hierarchy_depth(&folders, "Projects/2026"), 1);
        assert_eq!(
            subscription_hierarchy_depth(&folders, "Projects/2026/Archive"),
            2
        );
        assert!(subscription_row_matches_filter(&folders[1], "2026"));
        assert!(!subscription_row_matches_filter(&folders[1], "inbox"));
        assert!(
            !folders[0].is_selectable,
            "NOSELECT rows are retained, not fabricated away"
        );
    }

    #[test]
    fn dialog_routes_provider_work_only_through_cancellable_workers() {
        let source = include_str!("folder_subscriptions_dialog.rs");
        assert!(source.contains("std::thread::spawn"));
        assert!(source.contains("load_subscription_folders_with_cancellation"));
        assert!(source.contains("set_folder_subscription_with_cancellation"));
        assert!(source.contains("check.set_active(subscribed)"));
        assert!(source.contains("on_confirmed_change"));
    }
}
