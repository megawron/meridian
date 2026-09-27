// SPDX-License-Identifier: GPL-3.0-or-later
//
// ManageGroupsDialog — Modal dialog for organizing, reordering, toggling visibility,
// and creating/deleting custom source groups (upstream e_source_selector_manage_groups parity).

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::models::source_selector::SourceSelector;

pub struct ManageGroupsDialog {
    dialog: adw::Dialog,
    parent: Option<gtk::Widget>,
}

impl ManageGroupsDialog {
    pub fn new<P, F>(
        parent: Option<&P>,
        domain_label: &str,
        selector: &SourceSelector,
        on_save: F,
    ) -> Self
    where
        P: IsA<gtk::Widget>,
        F: Fn(SourceSelector) + 'static,
    {
        let dialog = adw::Dialog::builder()
            .title(format!("{domain_label}-Gruppen verwalten"))
            .content_width(480)
            .content_height(520)
            .build();

        let parent_widget = parent.map(|p| p.upcast_ref::<gtk::Widget>().clone());

        let working_selector = Rc::new(RefCell::new(selector.clone()));

        let toolbar = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let cancel_btn = gtk::Button::builder()
            .label("Abbrechen")
            .build();
        let dialog_cancel = dialog.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(w) = dialog_cancel.upgrade() {
                w.close();
            }
        });
        header_bar.pack_start(&cancel_btn);

        let save_btn = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_save = dialog.downgrade();
        let ws_save = Rc::clone(&working_selector);
        let on_save_cb = Rc::new(on_save);
        save_btn.connect_clicked(move |_| {
            let final_selector = ws_save.borrow().clone();
            on_save_cb(final_selector);
            if let Some(w) = dialog_save.upgrade() {
                w.close();
            }
        });
        header_bar.pack_end(&save_btn);

        toolbar.add_top_bar(&header_bar);

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();

        // 1. Groups preference group
        let groups_pref = adw::PreferencesGroup::builder()
            .title("Quellengruppen")
            .description("Legen Sie die Reihenfolge und Sichtbarkeit der Quellengruppen fest.")
            .build();

        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();

        rebuild_groups_list(&list_box, &working_selector, domain_label);
        groups_pref.add(&list_box);
        content_box.append(&groups_pref);

        // 2. Add custom group preference group
        let add_pref = adw::PreferencesGroup::builder()
            .title("Neue Gruppe")
            .description("Fügen Sie eine neue benutzerdefinierte Quellengruppe hinzu.")
            .build();

        let add_row = adw::EntryRow::builder()
            .title("Gruppenname")
            .show_apply_button(true)
            .build();

        let add_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Gruppe hinzufügen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        add_row.add_suffix(&add_btn);

        let ws_add = Rc::clone(&working_selector);
        let list_box_add = list_box.downgrade();
        let add_row_weak = add_row.downgrade();
        let dl_add = domain_label.to_string();

        let perform_add = Rc::new(move || {
            if let Some(entry) = add_row_weak.upgrade() {
                let name = entry.text().trim().to_string();
                if !name.is_empty() {
                    let mut sel = ws_add.borrow_mut();
                    if sel.add_custom_group(&name) {
                        drop(sel);
                        entry.set_text("");
                        if let Some(lb) = list_box_add.upgrade() {
                            rebuild_groups_list(&lb, &ws_add, &dl_add);
                        }
                    }
                }
            }
        });

        {
            let pa = Rc::clone(&perform_add);
            add_row.connect_apply(move |_| pa());
        }
        {
            let pa = Rc::clone(&perform_add);
            add_row.connect_entry_activated(move |_| pa());
        }
        {
            let pa = Rc::clone(&perform_add);
            add_btn.connect_clicked(move |_| pa());
        }

        add_pref.add(&add_row);
        content_box.append(&add_pref);

        // 3. Info notice
        let info_pref = adw::PreferencesGroup::builder()
            .description("Hinweis: Das Ausblenden einer Gruppe blendet alle darin enthaltenen Quellen aus und hebt deren Auswahl in den Ansichten auf.")
            .build();
        content_box.append(&info_pref);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&content_box)
            .build();

        toolbar.set_content(Some(&scrolled));
        dialog.set_child(Some(&toolbar));

        Self {
            dialog,
            parent: parent_widget,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn present(&self) {
        self.dialog.present(self.parent.as_ref());
    }
}

fn rebuild_groups_list(
    list_box: &gtk::ListBox,
    working_selector: &Rc<RefCell<SourceSelector>>,
    domain_label: &str,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let groups = working_selector.borrow().ordered_groups();
    let total = groups.len();

    if total == 0 {
        let empty_row = adw::ActionRow::builder()
            .title("Keine Gruppen vorhanden")
            .subtitle("Fügen Sie unten eine neue Gruppe hinzu.")
            .build();
        list_box.append(&empty_row);
        return;
    }

    for (index, group) in groups.into_iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(&group.name)
            .build();

        let count_text = if group.source_count == 1 {
            format!("1 {domain_label}")
        } else {
            format!("{} {domain_label}", group.source_count)
        };
        let subtitle = if group.is_custom {
            format!("{count_text} • Benutzerdefiniert")
        } else {
            count_text
        };
        row.set_subtitle(&subtitle);

        let icon = gtk::Image::builder()
            .icon_name(if group.is_custom {
                "folder-saved-search-symbolic"
            } else {
                "folder-symbolic"
            })
            .valign(gtk::Align::Center)
            .build();
        row.add_prefix(&icon);

        let controls_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .valign(gtk::Align::Center)
            .build();

        // Visibility Switch
        let switch = gtk::Switch::builder()
            .active(!group.is_hidden)
            .valign(gtk::Align::Center)
            .tooltip_text("Gruppe ein- oder ausblenden")
            .build();

        let ws_switch = Rc::clone(working_selector);
        let group_name_switch = group.name.clone();
        switch.connect_active_notify(move |sw| {
            ws_switch
                .borrow_mut()
                .set_group_hidden(&group_name_switch, !sw.is_active());
        });
        controls_box.append(&switch);

        // Move Up button
        let up_btn = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Gruppe nach oben verschieben")
            .sensitive(index > 0)
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();

        let ws_up = Rc::clone(working_selector);
        let lb_up = list_box.downgrade();
        let dl_up = domain_label.to_string();
        up_btn.connect_clicked(move |_| {
            if ws_up.borrow_mut().move_group_up(index) {
                if let Some(lb) = lb_up.upgrade() {
                    rebuild_groups_list(&lb, &ws_up, &dl_up);
                }
            }
        });
        controls_box.append(&up_btn);

        // Move Down button
        let down_btn = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Gruppe nach unten verschieben")
            .sensitive(index + 1 < total)
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();

        let ws_down = Rc::clone(working_selector);
        let lb_down = list_box.downgrade();
        let dl_down = domain_label.to_string();
        down_btn.connect_clicked(move |_| {
            if ws_down.borrow_mut().move_group_down(index) {
                if let Some(lb) = lb_down.upgrade() {
                    rebuild_groups_list(&lb, &ws_down, &dl_down);
                }
            }
        });
        controls_box.append(&down_btn);

        // If custom: Delete button
        if group.is_custom {
            let del_btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Benutzerdefinierte Gruppe löschen")
                .css_classes(vec!["flat", "circular", "destructive-action"])
                .valign(gtk::Align::Center)
                .build();

            let ws_del = Rc::clone(working_selector);
            let group_name_del = group.name.clone();
            let lb_del = list_box.downgrade();
            let dl_del = domain_label.to_string();
            del_btn.connect_clicked(move |_| {
                if ws_del.borrow_mut().delete_custom_group(&group_name_del) {
                    if let Some(lb) = lb_del.upgrade() {
                        rebuild_groups_list(&lb, &ws_del, &dl_del);
                    }
                }
            });
            controls_box.append(&del_btn);
        }

        row.add_suffix(&controls_box);
        list_box.append(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::ManageGroupsDialog;
    use crate::models::source_selector::{SourceDomain, SourceSelector, SourceSelectorItem};

    #[test]
    fn manage_groups_dialog_copy_and_labels() {
        let _ = std::any::type_name::<ManageGroupsDialog>();
        let source = include_str!("manage_groups_dialog.rs");
        assert!(source.contains("Quellengruppen"));
        assert!(source.contains("Legen Sie die Reihenfolge und Sichtbarkeit der Quellengruppen fest."));
        assert!(source.contains("Neue Gruppe"));
        assert!(source.contains("Hinweis: Das Ausblenden einer Gruppe blendet alle darin enthaltenen Quellen aus"));
        assert!(source.contains("Abbrechen"));
        assert!(source.contains("Speichern"));
    }

    #[test]
    fn manage_groups_working_selector_mutations() {
        let mut selector = SourceSelector::new(SourceDomain::Calendar);
        selector.update_sources(vec![
            SourceSelectorItem::new("c1", "Privat", "local", true, true).with_group("Auf diesem Computer"),
            SourceSelectorItem::new("c2", "Arbeit", "caldav", true, true).with_group("CalDAV Server"),
        ]);

        // Verify initial groups
        let groups = selector.ordered_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "Auf diesem Computer");
        assert_eq!(groups[1].name, "CalDAV Server");

        // Simulate dialog action: move down
        assert!(selector.move_group_down(0));
        let updated = selector.ordered_groups();
        assert_eq!(updated[0].name, "CalDAV Server");
        assert_eq!(updated[1].name, "Auf diesem Computer");

        // Simulate dialog action: hide group
        selector.set_group_hidden("CalDAV Server", true);
        assert!(selector.is_group_hidden("CalDAV Server"));
        assert!(!selector.is_group_hidden("Auf diesem Computer"));

        // Simulate dialog action: add custom group
        assert!(selector.add_custom_group("Verein"));
        let with_custom = selector.ordered_groups();
        assert_eq!(with_custom.len(), 3);
        assert_eq!(with_custom[2].name, "Verein");
        assert!(with_custom[2].is_custom);

        // Simulate dialog action: delete custom group
        assert!(selector.delete_custom_group("Verein"));
        assert_eq!(selector.ordered_groups().len(), 2);
    }
}
