// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable GTK4/libadwaita editor for a single sibling level in the mail tree.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::gettext;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderSortOrderItem {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FolderSortOrderOutcome {
    Save(Vec<String>),
    ResetCurrent,
    ResetAll,
}

/// Small, explicit list editor for one sibling level.  Evolution's original
/// dialog uses drag-and-drop; GTK4's accessible arrow controls provide the
/// same sibling-only operation without ever implying a server-side move.
pub struct FolderSortOrderDialog;

impl FolderSortOrderDialog {
    pub fn present<F>(
        parent: &impl IsA<gtk::Widget>,
        title: &str,
        items: Vec<FolderSortOrderItem>,
        on_outcome: F,
    ) where
        F: Fn(FolderSortOrderOutcome) + 'static,
    {
        let dialog = adw::Dialog::builder()
            .title(gettext("Folder Sort Order"))
            .content_width(460)
            .content_height(420)
            .build();
        let order = Rc::new(RefCell::new(items));
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["boxed-list"])
            .build();
        let selected = Rc::new(RefCell::new(None::<usize>));
        let refresh = Rc::new(RefCell::new(None::<Box<dyn Fn()>>));
        let rebuilding = Rc::new(Cell::new(false));

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder().label(gettext("Save")).build();
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(24)
            .margin_end(24)
            .margin_top(18)
            .margin_bottom(24)
            .build();
        content.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["title-4"])
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label(gettext("This order only applies to this folder level. Folders are not moved on the server."))
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );
        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        content.append(&scrolled);

        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text(gettext("Move Up"))
            .build();
        let down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text(gettext("Move Down"))
            .build();
        let reset_current = gtk::Button::with_label(&gettext("Reset This Level"));
        let reset_all = gtk::Button::with_label(&gettext("Reset All Levels"));
        controls.append(&reset_current);
        controls.append(&reset_all);
        controls.append(&up);
        controls.append(&down);
        content.append(&controls);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));

        let list_for_refresh = list.clone();
        let order_for_refresh = Rc::clone(&order);
        let selected_for_refresh = Rc::clone(&selected);
        let up_for_refresh = up.clone();
        let down_for_refresh = down.clone();
        let rebuilding_for_refresh = Rc::clone(&rebuilding);
        *refresh.borrow_mut() = Some(Box::new(move || {
            rebuilding_for_refresh.set(true);
            while let Some(row) = list_for_refresh.row_at_index(0) {
                list_for_refresh.remove(&row);
            }
            let current = *selected_for_refresh.borrow();
            for (index, item) in order_for_refresh.borrow().iter().enumerate() {
                let row_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(10)
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();
                let icon = gtk::Image::from_icon_name("folder-symbolic");
                let label = gtk::Label::builder()
                    .label(&item.label)
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                row_box.append(&icon);
                row_box.append(&label);
                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&row_box));
                list_for_refresh.append(&row);
                if current == Some(index) {
                    list_for_refresh.select_row(Some(&row));
                }
            }
            let len = order_for_refresh.borrow().len();
            up_for_refresh.set_sensitive(current.is_some_and(|index| index > 0));
            down_for_refresh.set_sensitive(current.is_some_and(|index| index + 1 < len));
            rebuilding_for_refresh.set(false);
        }));
        let refresh_now = Rc::clone(&refresh);
        refresh_now.borrow().as_ref().expect("refresh initialized")();

        let selected_for_selection = Rc::clone(&selected);
        let order_for_selection = Rc::clone(&order);
        let rebuilding_for_selection = Rc::clone(&rebuilding);
        let up_for_selection = up.clone();
        let down_for_selection = down.clone();
        list.connect_row_selected(move |_, row| {
            if rebuilding_for_selection.get() {
                return;
            }
            *selected_for_selection.borrow_mut() = row.map(|row| row.index() as usize);
            let current = *selected_for_selection.borrow();
            let len = order_for_selection.borrow().len();
            up_for_selection.set_sensitive(current.is_some_and(|index| index > 0));
            down_for_selection.set_sensitive(current.is_some_and(|index| index + 1 < len));
        });

        for (button, delta) in [(up, -1isize), (down, 1isize)] {
            let order = Rc::clone(&order);
            let selected = Rc::clone(&selected);
            let refresh = Rc::clone(&refresh);
            button.connect_clicked(move |_| {
                let Some(index) = *selected.borrow() else {
                    return;
                };
                let target = index as isize + delta;
                if !(0..order.borrow().len() as isize).contains(&target) {
                    return;
                }
                order.borrow_mut().swap(index, target as usize);
                *selected.borrow_mut() = Some(target as usize);
                refresh.borrow().as_ref().expect("refresh initialized")();
            });
        }

        let outcome = Rc::new(on_outcome);
        let outcome_for_save = Rc::clone(&outcome);
        let order_for_save = Rc::clone(&order);
        let dialog_for_save = dialog.clone();
        save.connect_clicked(move |_| {
            outcome_for_save(FolderSortOrderOutcome::Save(
                order_for_save
                    .borrow()
                    .iter()
                    .map(|item| item.id.clone())
                    .collect(),
            ));
            dialog_for_save.close();
        });
        for (button, value) in [
            (reset_current, FolderSortOrderOutcome::ResetCurrent),
            (reset_all, FolderSortOrderOutcome::ResetAll),
        ] {
            let outcome = Rc::clone(&outcome);
            let dialog = dialog.clone();
            button.connect_clicked(move |_| {
                outcome(value.clone());
                dialog.close();
            });
        }
        dialog.present(Some(parent));
    }
}

#[cfg(test)]
mod tests {
    use super::{FolderSortOrderItem, FolderSortOrderOutcome};

    #[test]
    fn outcome_keeps_the_explicit_sibling_order() {
        let items = ["A", "B", "C"]
            .into_iter()
            .map(|label| FolderSortOrderItem {
                id: label.to_string(),
                label: label.to_string(),
            })
            .collect::<Vec<_>>();
        let outcome = FolderSortOrderOutcome::Save(vec![
            items[2].id.clone(),
            items[0].id.clone(),
            items[1].id.clone(),
        ]);
        assert_eq!(
            outcome,
            FolderSortOrderOutcome::Save(vec!["C".into(), "A".into(), "B".into()])
        );
    }
}
