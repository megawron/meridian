// SPDX-License-Identifier: GPL-3.0-or-later
//
// SourceSelector — Unified multi-selection and primary source model for groupware sidebars.
// Upstream parity with src/e-util/e-source-selector.c and src/modules/calendar/e-cal-base-shell-sidebar.c.

use crate::services::settings_service::AppSettings;
use std::collections::{HashMap, HashSet};

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

    pub fn hidden_groups_key(&self) -> &'static str {
        match self {
            Self::Calendar => "calendar-hidden-groups",
            Self::Tasks => "task-hidden-groups",
            Self::Memos => "memo-hidden-groups",
            Self::Contacts => "addressbook-hidden-groups",
        }
    }

    pub fn groups_order_key(&self) -> &'static str {
        match self {
            Self::Calendar => "calendar-groups-order",
            Self::Tasks => "task-groups-order",
            Self::Memos => "memo-groups-order",
            Self::Contacts => "addressbook-groups-order",
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

    pub fn read_hidden_groups(&self, settings: &AppSettings) -> Vec<String> {
        match self {
            Self::Calendar => settings.calendar_hidden_groups(),
            Self::Tasks => settings.task_hidden_groups(),
            Self::Memos => settings.memo_hidden_groups(),
            Self::Contacts => settings.addressbook_hidden_groups(),
        }
    }

    pub fn write_hidden_groups(&self, settings: &AppSettings, groups: &[String]) {
        match self {
            Self::Calendar => settings.set_calendar_hidden_groups(groups),
            Self::Tasks => settings.set_task_hidden_groups(groups),
            Self::Memos => settings.set_memo_hidden_groups(groups),
            Self::Contacts => settings.set_addressbook_hidden_groups(groups),
        }
    }

    pub fn read_groups_order(&self, settings: &AppSettings) -> Vec<String> {
        match self {
            Self::Calendar => settings.calendar_groups_order(),
            Self::Tasks => settings.task_groups_order(),
            Self::Memos => settings.memo_groups_order(),
            Self::Contacts => settings.addressbook_groups_order(),
        }
    }

    pub fn write_groups_order(&self, settings: &AppSettings, groups: &[String]) {
        match self {
            Self::Calendar => settings.set_calendar_groups_order(groups),
            Self::Tasks => settings.set_task_groups_order(groups),
            Self::Memos => settings.set_memo_groups_order(groups),
            Self::Contacts => settings.set_addressbook_groups_order(groups),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    pub name: String,
    pub is_hidden: bool,
    pub source_count: usize,
    pub is_custom: bool,
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
    pub group_name: String,
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
        let group_name = if is_local || backend == "local" {
            "Auf diesem Computer".to_string()
        } else if !backend.is_empty() {
            backend.clone()
        } else {
            "Auf diesem Computer".to_string()
        };
        Self {
            uid,
            display_name: display_name.into(),
            backend_name: backend,
            color: None,
            enabled,
            writable,
            removable: !is_local,
            is_local,
            group_name,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group_name = group.into();
        self
    }
}

#[derive(Debug, Clone)]
pub struct SourceSelector {
    domain: SourceDomain,
    sources: Vec<SourceSelectorItem>,
    selected_uids: HashSet<String>,
    primary_uid: Option<String>,
    groups_order: Vec<String>,
    hidden_groups: HashSet<String>,
    custom_groups: Vec<String>,
}

impl SourceSelector {
    pub fn new(domain: SourceDomain) -> Self {
        Self {
            domain,
            sources: Vec::new(),
            selected_uids: HashSet::new(),
            primary_uid: None,
            groups_order: Vec::new(),
            hidden_groups: HashSet::new(),
            custom_groups: Vec::new(),
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
            if source.enabled && !self.hidden_groups.contains(&source.group_name) {
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

    // --- Group Management (upstream e_source_selector_manage_groups parity) ---

    pub fn groups_order(&self) -> &[String] {
        &self.groups_order
    }

    pub fn set_groups_order(&mut self, order: &[String]) {
        self.groups_order = order.to_vec();
    }

    pub fn hidden_groups(&self) -> Vec<String> {
        let mut list: Vec<_> = self.hidden_groups.iter().cloned().collect();
        list.sort();
        list
    }

    pub fn set_hidden_groups(&mut self, hidden: &[String]) {
        self.hidden_groups = hidden.iter().cloned().collect();
        self.deselect_sources_in_hidden_groups();
    }

    pub fn is_group_hidden(&self, name: &str) -> bool {
        self.hidden_groups.contains(name)
    }

    pub fn set_group_hidden(&mut self, name: &str, hidden: bool) {
        if hidden {
            self.hidden_groups.insert(name.to_string());
            self.deselect_sources_in_hidden_groups();
        } else {
            self.hidden_groups.remove(name);
        }
    }

    pub fn toggle_group_hidden(&mut self, name: &str) -> bool {
        let current = self.is_group_hidden(name);
        self.set_group_hidden(name, !current);
        !current
    }

    pub fn ordered_groups(&self) -> Vec<SourceGroup> {
        let mut known = Vec::new();
        // 1. Groups from sources
        for s in &self.sources {
            if !s.group_name.is_empty() && !known.contains(&s.group_name) {
                known.push(s.group_name.clone());
            }
        }
        // 2. Custom groups
        for cg in &self.custom_groups {
            if !known.contains(cg) {
                known.push(cg.clone());
            }
        }
        if known.is_empty() {
            known.push("Auf diesem Computer".to_string());
        }

        // Order according to self.groups_order
        let mut ordered_names = Vec::new();
        for og in &self.groups_order {
            if known.contains(og) && !ordered_names.contains(og) {
                ordered_names.push(og.clone());
            }
        }
        for k in &known {
            if !ordered_names.contains(k) {
                ordered_names.push(k.clone());
            }
        }

        ordered_names
            .into_iter()
            .map(|name| {
                let source_count = self.sources.iter().filter(|s| s.group_name == name).count();
                let is_custom = self.custom_groups.contains(&name);
                let is_hidden = self.hidden_groups.contains(&name);
                SourceGroup {
                    name,
                    is_hidden,
                    source_count,
                    is_custom,
                }
            })
            .collect()
    }

    pub fn move_group_up(&mut self, index: usize) -> bool {
        let mut groups: Vec<String> = self.ordered_groups().into_iter().map(|g| g.name).collect();
        if index > 0 && index < groups.len() {
            groups.swap(index, index - 1);
            self.groups_order = groups;
            true
        } else {
            false
        }
    }

    pub fn move_group_down(&mut self, index: usize) -> bool {
        let mut groups: Vec<String> = self.ordered_groups().into_iter().map(|g| g.name).collect();
        if index + 1 < groups.len() {
            groups.swap(index, index + 1);
            self.groups_order = groups;
            true
        } else {
            false
        }
    }

    pub fn add_custom_group(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let groups = self.ordered_groups();
        if groups.iter().any(|g| g.name.eq_ignore_ascii_case(name)) {
            return false;
        }
        self.custom_groups.push(name.to_string());
        self.groups_order.push(name.to_string());
        true
    }

    pub fn rename_group(&mut self, old_name: &str, new_name: &str) -> bool {
        let new_name = new_name.trim();
        if new_name.is_empty() || old_name == new_name {
            return false;
        }
        let groups = self.ordered_groups();
        if groups
            .iter()
            .any(|g| g.name.eq_ignore_ascii_case(new_name) && g.name != old_name)
        {
            return false;
        }

        for s in &mut self.sources {
            if s.group_name == old_name {
                s.group_name = new_name.to_string();
            }
        }
        for g in &mut self.custom_groups {
            if g == old_name {
                *g = new_name.to_string();
            }
        }
        for g in &mut self.groups_order {
            if g == old_name {
                *g = new_name.to_string();
            }
        }
        if self.hidden_groups.remove(old_name) {
            self.hidden_groups.insert(new_name.to_string());
        }
        true
    }

    pub fn delete_custom_group(&mut self, name: &str) -> bool {
        self.custom_groups.retain(|g| g != name);
        self.groups_order.retain(|g| g != name);
        self.hidden_groups.remove(name);
        for s in &mut self.sources {
            if s.group_name == name {
                s.group_name = "Auf diesem Computer".to_string();
            }
        }
        true
    }

    /// Deselect all sources in hidden groups (upstream parity with deselect_sources_in_hidden_groups)
    pub fn deselect_sources_in_hidden_groups(&mut self) {
        if self.hidden_groups.is_empty() {
            return;
        }
        for s in &self.sources {
            if self.hidden_groups.contains(&s.group_name) {
                self.selected_uids.remove(&s.uid);
            }
        }
    }

    /// Return visible sources (sources not belonging to hidden groups)
    pub fn visible_sources(&self) -> Vec<&SourceSelectorItem> {
        self.sources
            .iter()
            .filter(|s| !self.hidden_groups.contains(&s.group_name))
            .collect()
    }

    /// Update source metadata (display name, color, default) atomically
    pub fn update_source_metadata(
        &mut self,
        uid: &str,
        display_name: &str,
        color: Option<&str>,
        is_default: bool,
    ) -> bool {
        if let Some(source) = self.sources.iter_mut().find(|s| s.uid == uid) {
            let trimmed = display_name.trim();
            if !trimmed.is_empty() {
                source.display_name = trimmed.to_string();
            }
            if let Some(c) = color {
                source.color = Some(c.to_string());
            }
            if is_default {
                self.primary_uid = Some(uid.to_string());
                self.selected_uids.insert(uid.to_string());
            }
            true
        } else {
            false
        }
    }

    pub fn load_from_settings(&mut self, settings: &AppSettings) {
        let hidden = self.domain.read_hidden_sources(settings);
        let hidden_set: HashSet<&str> = hidden.iter().map(String::as_str).collect();
        let default_uid = self.domain.read_default_source(settings);

        let hidden_groups = self.domain.read_hidden_groups(settings);
        self.hidden_groups = hidden_groups.into_iter().collect();

        let groups_order = self.domain.read_groups_order(settings);
        self.groups_order = groups_order;

        self.selected_uids.clear();
        for source in &self.sources {
            if source.enabled
                && !hidden_set.contains(source.uid.as_str())
                && !self.hidden_groups.contains(&source.group_name)
            {
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

        self.deselect_sources_in_hidden_groups();
    }

    pub fn save_to_settings(&self, settings: &AppSettings) {
        let hidden = self.hidden_uids();
        self.domain.write_hidden_sources(settings, &hidden);
        if let Some(primary) = &self.primary_uid {
            self.domain.write_default_source(settings, primary);
        }
        let hidden_groups: Vec<String> = self.hidden_groups.iter().cloned().collect();
        self.domain.write_hidden_groups(settings, &hidden_groups);
        if self.groups_order.is_empty() {
            self.domain.write_groups_order(settings, &[]);
        } else {
            let groups: Vec<String> = self.ordered_groups().into_iter().map(|g| g.name).collect();
            self.domain.write_groups_order(settings, &groups);
        }
    }

    pub fn update_sources(&mut self, mut new_sources: Vec<SourceSelectorItem>) {
        let old_selected = self.selected_uids.clone();
        let had_sources = !self.sources.is_empty();

        let old_metadata: HashMap<String, (String, Option<String>, String)> = self
            .sources
            .iter()
            .map(|s| {
                (
                    s.uid.clone(),
                    (
                        s.display_name.clone(),
                        s.color.clone(),
                        s.group_name.clone(),
                    ),
                )
            })
            .collect();

        for source in &mut new_sources {
            if let Some((old_name, old_color, old_group)) = old_metadata.get(&source.uid) {
                if !old_name.is_empty() {
                    source.display_name = old_name.clone();
                }
                if old_color.is_some() {
                    source.color = old_color.clone();
                }
                if !old_group.is_empty() && old_group != "On This Computer" {
                    source.group_name = old_group.clone();
                }
            }
        }

        self.sources = new_sources;

        let mut updated_selected = HashSet::new();
        for source in &self.sources {
            if !source.enabled || self.hidden_groups.contains(&source.group_name) {
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

pub fn create_source_context_popover<FExcl, FAll, FDefault, FProps, FGroups>(
    is_primary: bool,
    domain_label: &str,
    on_exclusive: FExcl,
    on_select_all: FAll,
    on_set_default: FDefault,
    on_properties: Option<FProps>,
    on_manage_groups: Option<FGroups>,
) -> gtk::Popover
where
    FExcl: Fn() + 'static,
    FAll: Fn() + 'static,
    FDefault: Fn() + 'static,
    FProps: Fn() + 'static,
    FGroups: Fn() + 'static,
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

    if on_properties.is_some() || on_manage_groups.is_some() {
        popover_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    if let Some(on_properties) = on_properties {
        let btn_props = gtk::Button::builder()
            .label("Eigenschaften…")
            .css_classes(vec!["flat"])
            .build();
        let pop_weak4 = popover.downgrade();
        btn_props.connect_clicked(move |_| {
            if let Some(p) = pop_weak4.upgrade() {
                p.popdown();
            }
            on_properties();
        });
        popover_box.append(&btn_props);
    }

    if let Some(on_manage_groups) = on_manage_groups {
        let btn_groups = gtk::Button::builder()
            .label("Gruppen verwalten…")
            .css_classes(vec!["flat"])
            .build();
        let pop_weak5 = popover.downgrade();
        btn_groups.connect_clicked(move |_| {
            if let Some(p) = pop_weak5.upgrade() {
                p.popdown();
            }
            on_manage_groups();
        });
        popover_box.append(&btn_groups);
    }

    popover
}

pub fn create_source_row_menu_full<FExcl, FAll, FDefault, FProps, FGroups>(
    is_primary: bool,
    domain_label: &str,
    on_exclusive: FExcl,
    on_select_all: FAll,
    on_set_default: FDefault,
    on_properties: Option<FProps>,
    on_manage_groups: Option<FGroups>,
) -> gtk::MenuButton
where
    FExcl: Fn() + 'static,
    FAll: Fn() + 'static,
    FDefault: Fn() + 'static,
    FProps: Fn() + 'static,
    FGroups: Fn() + 'static,
{
    let popover = create_source_context_popover(
        is_primary,
        domain_label,
        on_exclusive,
        on_select_all,
        on_set_default,
        on_properties,
        on_manage_groups,
    );

    gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .popover(&popover)
        .css_classes(vec!["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text("Optionen")
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
    create_source_row_menu_full(
        is_primary,
        domain_label,
        on_exclusive,
        on_select_all,
        on_set_default,
        None::<fn()>,
        None::<fn()>,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn sample_sources() -> Vec<SourceSelectorItem> {
        vec![
            SourceSelectorItem::new("local-cal", "Persönlicher Kalender", "local", true, true),
            SourceSelectorItem::new("work-cal", "Arbeitskalender", "caldav", true, true)
                .with_color("#3465a4")
                .with_group("Arbeit"),
            SourceSelectorItem::new("holiday-cal", "Feiertage", "webcal", true, false)
                .with_color("#4e9a06")
                .with_group("Abonnements"),
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
    }

    #[test]
    fn group_ordering_and_movement() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        let groups = selector.ordered_groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].name, "Auf diesem Computer");
        assert_eq!(groups[1].name, "Arbeit");
        assert_eq!(groups[2].name, "Abonnements");

        // Move "Arbeit" (index 1) up
        assert!(selector.move_group_up(1));
        let groups_after_up = selector.ordered_groups();
        assert_eq!(groups_after_up[0].name, "Arbeit");
        assert_eq!(groups_after_up[1].name, "Auf diesem Computer");
        assert_eq!(groups_after_up[2].name, "Abonnements");

        // Cannot move index 0 up
        assert!(!selector.move_group_up(0));

        // Move index 0 down
        assert!(selector.move_group_down(0));
        let groups_after_down = selector.ordered_groups();
        assert_eq!(groups_after_down[0].name, "Auf diesem Computer");
        assert_eq!(groups_after_down[1].name, "Arbeit");

        // Cannot move last index down
        assert!(!selector.move_group_down(2));
    }

    #[test]
    fn custom_group_addition_renaming_and_deletion() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        assert!(selector.add_custom_group("Verein"));
        // Cannot add empty or duplicate
        assert!(!selector.add_custom_group(""));
        assert!(!selector.add_custom_group("verein"));

        let groups = selector.ordered_groups();
        assert_eq!(groups.len(), 4);
        assert!(groups.iter().any(|g| g.name == "Verein" && g.is_custom));

        // Rename "Verein" -> "Sportverein"
        assert!(selector.rename_group("Verein", "Sportverein"));
        let groups2 = selector.ordered_groups();
        assert!(groups2.iter().any(|g| g.name == "Sportverein"));
        assert!(!groups2.iter().any(|g| g.name == "Verein"));

        // Delete custom group
        assert!(selector.delete_custom_group("Sportverein"));
        let groups3 = selector.ordered_groups();
        assert_eq!(groups3.len(), 3);
        assert!(!groups3.iter().any(|g| g.name == "Sportverein"));
    }

    #[test]
    fn group_visibility_and_deselection() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        assert!(selector.is_selected("work-cal"));
        assert_eq!(selector.visible_sources().len(), 4);

        // Hide "Arbeit" group
        selector.set_group_hidden("Arbeit", true);
        assert!(selector.is_group_hidden("Arbeit"));
        // Upstream deselect_sources_in_hidden_groups unselects sources in that group
        assert!(!selector.is_selected("work-cal"));

        let visible = selector.visible_sources();
        assert_eq!(visible.len(), 3);
        assert!(!visible.iter().any(|s| s.uid == "work-cal"));

        // Toggle back to visible
        assert!(!selector.toggle_group_hidden("Arbeit")); // returns new hidden state = false
        assert!(!selector.is_group_hidden("Arbeit"));
        assert_eq!(selector.visible_sources().len(), 4);
    }

    #[test]
    fn source_metadata_update() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(sample_sources());

        assert!(selector.update_source_metadata(
            "work-cal",
            "Hauptberuflicher Kalender",
            Some("#cc0000"),
            true, // set as default
        ));

        let updated = selector.source_by_uid("work-cal").unwrap();
        assert_eq!(updated.display_name, "Hauptberuflicher Kalender");
        assert_eq!(updated.color, Some("#cc0000".to_string()));
        assert_eq!(selector.primary_uid(), Some("work-cal"));
        assert!(selector.is_selected("work-cal"));

        // Non-existent UID returns false
        assert!(!selector.update_source_metadata("unknown-uid", "Test", None, false));

        // update_sources preserves custom metadata for known UIDs
        let fresh_discovery = vec![
            SourceSelectorItem::new("work-cal", "Work", "caldav", true, false),
            SourceSelectorItem::new("new-cal", "New Calendar", "caldav", true, false),
        ];
        selector.update_sources(fresh_discovery);
        let preserved = selector.source_by_uid("work-cal").unwrap();
        assert_eq!(preserved.display_name, "Hauptberuflicher Kalender");
        assert_eq!(preserved.color, Some("#cc0000".to_string()));
    }

    #[test]
    fn settings_roundtrip_sync() {
        let backend = gio::memory_settings_backend_new();
        if let Some(settings) = AppSettings::try_new_with_backend(&backend) {
            let mut selector = SourceSelector::new(SourceDomain::Calendar);
            selector.update_sources(sample_sources());

            // Select only local-cal, set it as primary, reorder groups, hide "Abonnements"
            selector.select_exclusive("local-cal");
            selector.set_primary("local-cal");
            selector.set_group_hidden("Abonnements", true);
            selector.set_groups_order(&[
                "Arbeit".to_string(),
                "Auf diesem Computer".to_string(),
                "Abonnements".to_string(),
            ]);
            selector.save_to_settings(&settings);

            let mut fresh = SourceSelector::new(SourceDomain::Calendar);
            fresh.update_sources(sample_sources());
            fresh.load_from_settings(&settings);

            assert!(fresh.is_selected("local-cal"));
            assert!(!fresh.is_selected("work-cal"));
            assert_eq!(fresh.primary_uid(), Some("local-cal"));
            assert!(fresh.is_group_hidden("Abonnements"));
            assert_eq!(
                fresh.groups_order(),
                &[
                    "Arbeit".to_string(),
                    "Auf diesem Computer".to_string(),
                    "Abonnements".to_string()
                ]
            );

            // Reset back
            fresh.set_group_hidden("Abonnements", false);
            fresh.select_all();
            fresh.set_groups_order(&[]);
            fresh.save_to_settings(&settings);
        }
    }
}
