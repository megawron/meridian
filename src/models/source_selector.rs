// SPDX-License-Identifier: GPL-3.0-or-later
//
// SourceSelector — Unified multi-selection and primary source model for groupware sidebars.
// Upstream parity with src/e-util/e-source-selector.c and src/modules/calendar/e-cal-base-shell-sidebar.c.

use crate::services::settings_service::AppSettings;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceDomain {
    Calendar,
    Tasks,
    Memos,
    Contacts,
}

impl SourceDomain {
    pub fn hidden_sources_key(&self) -> &'static str {
        match self {
            Self::Calendar => "calendar-hidden-sources",
            Self::Tasks => "task-hidden-sources",
            Self::Memos => "memo-hidden-sources",
            Self::Contacts => "addressbook-hidden-sources",
        }
    }

    pub fn default_source_key(&self) -> &'static str {
        match self {
            Self::Calendar => "default-calendar-source",
            Self::Tasks => "default-task-source",
            Self::Memos => "default-memo-source",
            Self::Contacts => "default-address-book-source",
        }
    }

    pub fn read_hidden_sources(&self, settings: &AppSettings) -> Vec<String> {
        match self {
            Self::Calendar => settings.calendar_hidden_sources(),
            Self::Tasks => settings.task_hidden_sources(),
            Self::Memos => settings.memo_hidden_sources(),
            Self::Contacts => settings.addressbook_hidden_sources(),
        }
    }

    pub fn write_hidden_sources(&self, settings: &AppSettings, uids: &[String]) {
        match self {
            Self::Calendar => settings.set_calendar_hidden_sources(uids),
            Self::Tasks => settings.set_task_hidden_sources(uids),
            Self::Memos => settings.set_memo_hidden_sources(uids),
            Self::Contacts => settings.set_addressbook_hidden_sources(uids),
        }
    }

    pub fn read_default_source(&self, settings: &AppSettings) -> String {
        match self {
            Self::Calendar => settings.default_calendar_source_uid(),
            Self::Tasks => settings.default_task_source_uid(),
            Self::Memos => settings.default_memo_source_uid(),
            Self::Contacts => settings.default_address_book_source_uid(),
        }
    }

    pub fn write_default_source(&self, settings: &AppSettings, uid: &str) {
        match self {
            Self::Calendar => settings.set_default_calendar_source_uid(uid),
            Self::Tasks => settings.set_default_task_source_uid(uid),
            Self::Memos => settings.set_default_memo_source_uid(uid),
            Self::Contacts => settings.set_default_address_book_source_uid(uid),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelectorItem {
    pub uid: String,
    pub display_name: String,
    pub backend_name: String,
    pub color: Option<String>,
    pub enabled: bool,
    pub writable: bool,
    pub removable: bool,
    pub is_local: bool,
}

impl SourceSelectorItem {
    pub fn new(
        uid: impl Into<String>,
        display_name: impl Into<String>,
        backend_name: impl Into<String>,
        enabled: bool,
        writable: bool,
    ) -> Self {
        let uid = uid.into();
        let backend = backend_name.into();
        let is_local = uid.starts_with("local-") || backend == "local";
        Self {
            uid,
            display_name: display_name.into(),
            backend_name: backend,
            color: None,
            enabled,
            writable,
            removable: !is_local,
            is_local,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct SourceSelector {
    domain: SourceDomain,
    sources: Vec<SourceSelectorItem>,
    selected_uids: HashSet<String>,
    primary_uid: Option<String>,
}

impl SourceSelector {
    pub fn new(domain: SourceDomain) -> Self {
        Self {
            domain,
            sources: Vec::new(),
            selected_uids: HashSet::new(),
            primary_uid: None,
        }
    }

    pub fn domain(&self) -> SourceDomain {
        self.domain
    }

    pub fn sources(&self) -> &[SourceSelectorItem] {
        &self.sources
    }

    pub fn source_by_uid(&self, uid: &str) -> Option<&SourceSelectorItem> {
        self.sources.iter().find(|s| s.uid == uid)
    }

    pub fn is_selected(&self, uid: &str) -> bool {
        self.selected_uids.contains(uid)
    }

    pub fn set_selected(&mut self, uid: &str, selected: bool) -> bool {
        if self.sources.iter().any(|s| s.uid == uid) {
            if selected {
                self.selected_uids.insert(uid.to_string())
            } else {
                self.selected_uids.remove(uid)
            }
        } else {
            false
        }
    }

    pub fn toggle_selected(&mut self, uid: &str) -> bool {
        let current = self.is_selected(uid);
        self.set_selected(uid, !current);
        !current
    }

    pub fn select_all(&mut self) {
        for source in &self.sources {
            if source.enabled {
                self.selected_uids.insert(source.uid.clone());
            }
        }
    }

    pub fn unselect_all(&mut self) {
        self.selected_uids.clear();
    }

    pub fn select_exclusive(&mut self, uid: &str) {
        self.selected_uids.clear();
        if self.sources.iter().any(|s| s.uid == uid && s.enabled) {
            self.selected_uids.insert(uid.to_string());
        }
    }

    pub fn count_selected(&self) -> usize {
        self.selected_uids.len()
    }

    pub fn selected_uids(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|s| self.selected_uids.contains(&s.uid))
            .map(|s| s.uid.clone())
            .collect()
    }

    pub fn hidden_uids(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|s| !self.selected_uids.contains(&s.uid))
            .map(|s| s.uid.clone())
            .collect()
    }

    pub fn primary_uid(&self) -> Option<&str> {
        self.primary_uid.as_deref()
    }

    pub fn is_primary(&self, uid: &str) -> bool {
        self.primary_uid.as_deref() == Some(uid)
    }

    pub fn set_primary(&mut self, uid: &str) -> bool {
        if self.sources.iter().any(|s| s.uid == uid && s.enabled) {
            self.primary_uid = Some(uid.to_string());
            // Upstream primary source is always visible/selected
            self.selected_uids.insert(uid.to_string());
            true
        } else {
            false
        }
    }

    pub fn primary_source(&self) -> Option<&SourceSelectorItem> {
        self.primary_uid
            .as_deref()
            .and_then(|uid| self.source_by_uid(uid))
    }

    pub fn first_writable_source(&self) -> Option<&SourceSelectorItem> {
        // Prefer primary if writable and enabled
        if let Some(primary) = self.primary_source() {
            if primary.writable && primary.enabled {
                return Some(primary);
            }
        }
        self.sources
            .iter()
            .find(|s| s.enabled && s.writable && self.selected_uids.contains(&s.uid))
            .or_else(|| self.sources.iter().find(|s| s.enabled && s.writable))
    }

    pub fn load_from_settings(&mut self, settings: &AppSettings) {
        let hidden = self.domain.read_hidden_sources(settings);
        let hidden_set: HashSet<&str> = hidden.iter().map(String::as_str).collect();
        let default_uid = self.domain.read_default_source(settings);

        self.selected_uids.clear();
        for source in &self.sources {
            if source.enabled && !hidden_set.contains(source.uid.as_str()) {
                self.selected_uids.insert(source.uid.clone());
            }
        }

        if !default_uid.is_empty()
            && self
                .sources
                .iter()
                .any(|s| s.uid == default_uid && s.enabled)
        {
            self.primary_uid = Some(default_uid.clone());
            self.selected_uids.insert(default_uid);
        } else if self.primary_uid.is_none() {
            self.primary_uid = self
                .sources
                .iter()
                .find(|s| s.is_local && s.enabled)
                .or_else(|| self.sources.iter().find(|s| s.enabled))
                .map(|s| s.uid.clone());
        }
    }

    pub fn save_to_settings(&self, settings: &AppSettings) {
        let hidden = self.hidden_uids();
        self.domain.write_hidden_sources(settings, &hidden);
        if let Some(primary) = &self.primary_uid {
            self.domain.write_default_source(settings, primary);
        }
    }

    pub fn update_sources(&mut self, new_sources: Vec<SourceSelectorItem>) {
        let old_selected = self.selected_uids.clone();
        let had_sources = !self.sources.is_empty();

        self.sources = new_sources;

        let mut updated_selected = HashSet::new();
        for source in &self.sources {
            if !source.enabled {
                continue;
            }
            if had_sources {
                if old_selected.contains(&source.uid) {
                    updated_selected.insert(source.uid.clone());
                }
            } else {
                updated_selected.insert(source.uid.clone());
            }
        }
        self.selected_uids = updated_selected;

        let primary_valid = self
            .primary_uid
            .as_ref()
            .is_some_and(|p| self.sources.iter().any(|s| &s.uid == p && s.enabled));

        if !primary_valid {
            self.primary_uid = self
                .sources
                .iter()
                .find(|s| s.is_local && s.enabled)
                .or_else(|| self.sources.iter().find(|s| s.enabled))
                .map(|s| s.uid.clone());
        }

        if let Some(primary) = &self.primary_uid {
            self.selected_uids.insert(primary.clone());
        }
    }
}

use gtk::prelude::*;

pub fn create_primary_indicator() -> gtk::Image {
    gtk::Image::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Standardquelle")
        .css_classes(vec!["dim-label"])
        .valign(gtk::Align::Center)
        .build()
}

pub fn create_source_row_menu<FExcl, FAll, FDefault>(
    is_primary: bool,
    domain_label: &str,
    on_exclusive: FExcl,
    on_select_all: FAll,
    on_set_default: FDefault,
) -> gtk::MenuButton
where
    FExcl: Fn() + 'static,
    FAll: Fn() + 'static,
    FDefault: Fn() + 'static,
{
    let popover_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    let popover = gtk::Popover::builder().child(&popover_box).build();

    let btn_exclusive = gtk::Button::builder()
        .label(format!("Nur diese(n) {domain_label} anzeigen"))
        .css_classes(vec!["flat"])
        .build();
    let pop_weak = popover.downgrade();
    btn_exclusive.connect_clicked(move |_| {
        if let Some(p) = pop_weak.upgrade() {
            p.popdown();
        }
        on_exclusive();
    });
    popover_box.append(&btn_exclusive);

    let btn_all = gtk::Button::builder()
        .label(format!("Alle {domain_label} anzeigen"))
        .css_classes(vec!["flat"])
        .build();
    let pop_weak2 = popover.downgrade();
    btn_all.connect_clicked(move |_| {
        if let Some(p) = pop_weak2.upgrade() {
            p.popdown();
        }
        on_select_all();
    });
    popover_box.append(&btn_all);

    if !is_primary {
        let btn_default = gtk::Button::builder()
            .label("Als Standard festlegen")
            .css_classes(vec!["flat"])
            .build();
        let pop_weak3 = popover.downgrade();
        btn_default.connect_clicked(move |_| {
            if let Some(p) = pop_weak3.upgrade() {
                p.popdown();
            }
            on_set_default();
        });
        popover_box.append(&btn_default);
    }

    gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .popover(&popover)
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text("Optionen")
        .build()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn sample_sources() -> Vec<SourceSelectorItem> {
        vec![
            SourceSelectorItem::new("local-cal", "Persönlicher Kalender", "local", true, true),
            SourceSelectorItem::new("work-cal", "Arbeitskalender", "caldav", true, true)
                .with_color("#3465a4"),
            SourceSelectorItem::new("holiday-cal", "Feiertage", "webcal", true, false)
                .with_color("#4e9a06"),
            SourceSelectorItem::new("archived-cal", "Archiv", "local", false, false),
        ]
    }

    #[test]
    fn initial_sources_default_to_enabled_selection() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        assert_eq!(selector.count_selected(), 3);
        assert!(selector.is_selected("local-cal"));
        assert!(selector.is_selected("work-cal"));
        assert!(selector.is_selected("holiday-cal"));
        assert!(!selector.is_selected("archived-cal")); // disabled

        // Primary defaulted to local source
        assert_eq!(selector.primary_uid(), Some("local-cal"));
        assert!(selector.is_primary("local-cal"));
    }

    #[test]
    fn exclusive_selection_unchecks_all_others_and_selects_target() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        selector.select_exclusive("work-cal");
        assert_eq!(selector.count_selected(), 1);
        assert!(selector.is_selected("work-cal"));
        assert!(!selector.is_selected("local-cal"));
        assert!(!selector.is_selected("holiday-cal"));
    }

    #[test]
    fn select_all_and_unselect_all() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        selector.unselect_all();
        assert_eq!(selector.count_selected(), 0);
        assert!(selector.selected_uids().is_empty());
        assert_eq!(selector.hidden_uids().len(), 4);

        selector.select_all();
        // Only enabled sources are selected
        assert_eq!(selector.count_selected(), 3);
        assert!(!selector.is_selected("archived-cal"));
    }

    #[test]
    fn setting_primary_ensures_it_is_selected() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        selector.unselect_all();
        assert!(!selector.is_selected("work-cal"));

        assert!(selector.set_primary("work-cal"));
        assert_eq!(selector.primary_uid(), Some("work-cal"));
        assert!(selector.is_selected("work-cal")); // automatically included

        // Setting an unknown or disabled source as primary fails
        assert!(!selector.set_primary("non-existent"));
        assert!(!selector.set_primary("archived-cal"));
        assert_eq!(selector.primary_uid(), Some("work-cal"));
    }

    #[test]
    fn first_writable_source_prefers_primary() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        selector.set_primary("work-cal");
        let writable = selector
            .first_writable_source()
            .expect("should find writable");
        assert_eq!(writable.uid, "work-cal");

        // When holiday-cal is primary (read-only), falls back to writable selected
        selector.set_primary("holiday-cal");
        let fallback = selector
            .first_writable_source()
            .expect("should find fallback");
        assert!(fallback.writable);
        assert_ne!(fallback.uid, "holiday-cal");
    }

    #[test]
    fn external_update_preserves_existing_selections_and_replaces_primary_if_removed() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        // Custom state: only work-cal selected, work-cal is primary
        selector.select_exclusive("work-cal");
        selector.set_primary("work-cal");

        // External update: work-cal is removed, team-cal added
        let updated = vec![
            SourceSelectorItem::new("local-cal", "Persönlicher Kalender", "local", true, true),
            SourceSelectorItem::new("team-cal", "Team Kalender", "caldav", true, true),
        ];
        selector.update_sources(updated);

        // Previous selection for local-cal was false, so it remains unselected unless made primary
        // Primary work-cal is gone, so primary falls back to local-cal, making it selected
        assert_eq!(selector.primary_uid(), Some("local-cal"));
        assert!(selector.is_selected("local-cal"));
        // team-cal was not in old sources, so in an incremental update of existing selector, it doesn't break
    }

    #[test]
    fn settings_roundtrip_sync() {
        if let Some(settings) = AppSettings::try_new() {
            let mut selector = SourceSelector::new(SourceDomain::Calendar);
            selector.update_sources(sample_sources());

            // Select only local-cal, set it as primary
            selector.select_exclusive("local-cal");
            selector.set_primary("local-cal");
            selector.save_to_settings(&settings);

            let mut fresh = SourceSelector::new(SourceDomain::Calendar);
            fresh.update_sources(sample_sources());
            fresh.load_from_settings(&settings);

            assert!(fresh.is_selected("local-cal"));
            assert!(!fresh.is_selected("work-cal"));
            assert_eq!(fresh.primary_uid(), Some("local-cal"));

            // Reset back
            fresh.select_all();
            fresh.save_to_settings(&settings);
        }
    }
}
