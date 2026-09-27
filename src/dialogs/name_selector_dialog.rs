// SPDX-License-Identifier: GPL-3.0-or-later
//
// NameSelectorDialog — Evolution-compatible contact selection dialog.
//
// Parity with Evolution's upstream `src/e-util/e-name-selector-dialog.c`:
// Allows searching address books, selecting contacts, and transferring them
// to To ("An ->"), Cc ("Cc ->"), and Bcc ("Bcc ->") destination lists.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::services::contact_store_service::ContactStoreService;
use crate::services::vcard_service::VCardContact;

/// An item in the destination lists (To, Cc, Bcc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSelectorDestination {
    pub display_name: String,
    pub email: String,
}

impl NameSelectorDestination {
    pub fn new(display_name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into().trim().to_string(),
            email: email.into().trim().to_string(),
        }
    }

    pub fn formatted(&self) -> String {
        if self.display_name.is_empty() {
            self.email.clone()
        } else if self.email.is_empty() {
            self.display_name.clone()
        } else {
            format!("{} <{}>", self.display_name, self.email)
        }
    }
}

/// Parses comma- or semicolon-separated recipient strings into destinations.
pub fn parse_destinations(text: &str) -> Vec<NameSelectorDestination> {
    let mut results = Vec::new();
    for token in text.split(|c| c == ',' || c == ';') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let (Some(open), Some(close)) = (token.find('<'), token.rfind('>')) {
            if close > open {
                let name = token[..open].trim().trim_matches('"').trim();
                let email = token[open + 1..close].trim();
                results.push(NameSelectorDestination::new(name, email));
                continue;
            }
        }
        if token.contains('@') {
            results.push(NameSelectorDestination::new("", token));
        } else {
            results.push(NameSelectorDestination::new(token, ""));
        }
    }
    results
}

/// Formats a list of destinations into a comma-separated recipient string.
pub fn format_destinations(destinations: &[NameSelectorDestination]) -> String {
    destinations
        .iter()
        .map(|d| d.formatted())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Filter contact records by search query (case-insensitive substring of name, email, org, nickname).
pub fn filter_contacts<'a>(
    contacts: &'a [VCardContact],
    query: &str,
) -> Vec<&'a VCardContact> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return contacts.iter().collect();
    }
    contacts
        .iter()
        .filter(|contact| {
            if contact.fn_name.to_lowercase().contains(&query) {
                return true;
            }
            if contact.email.to_lowercase().contains(&query) {
                return true;
            }
            if contact
                .emails
                .iter()
                .any(|e| e.to_lowercase().contains(&query))
            {
                return true;
            }
            if contact.nickname.to_lowercase().contains(&query) {
                return true;
            }
            if contact.org.to_lowercase().contains(&query) {
                return true;
            }
            if contact.is_list && contact.list_name.to_lowercase().contains(&query) {
                return true;
            }
            false
        })
        .collect()
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct NameSelectorDialog {
        pub contacts: RefCell<Vec<VCardContact>>,
        pub destinations_to: RefCell<Vec<NameSelectorDestination>>,
        pub destinations_cc: RefCell<Vec<NameSelectorDestination>>,
        pub destinations_bcc: RefCell<Vec<NameSelectorDestination>>,
        pub callback: RefCell<Option<Box<dyn FnOnce(String, String, String)>>>,
    }

    impl std::fmt::Debug for NameSelectorDialog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NameSelectorDialog")
                .field("contacts", &self.contacts)
                .field("destinations_to", &self.destinations_to)
                .field("destinations_cc", &self.destinations_cc)
                .field("destinations_bcc", &self.destinations_bcc)
                .finish_non_exhaustive()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NameSelectorDialog {
        const NAME: &'static str = "EvolutionNameSelectorDialog";
        type Type = super::NameSelectorDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for NameSelectorDialog {}
    impl WidgetImpl for NameSelectorDialog {}
    impl AdwDialogImpl for NameSelectorDialog {}
}

glib::wrapper! {
    pub struct NameSelectorDialog(ObjectSubclass<imp::NameSelectorDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl NameSelectorDialog {
    pub fn new<F>(
        initial_to: &str,
        initial_cc: &str,
        initial_bcc: &str,
        on_apply: F,
    ) -> Self
    where
        F: FnOnce(String, String, String) + 'static,
    {
        let dialog: Self = glib::Object::builder()
            .property("title", "Empfänger auswählen")
            .property("content-width", 780)
            .property("content-height", 540)
            .build();

        dialog.imp().destinations_to.replace(parse_destinations(initial_to));
        dialog.imp().destinations_cc.replace(parse_destinations(initial_cc));
        dialog.imp().destinations_bcc.replace(parse_destinations(initial_bcc));
        dialog.imp().callback.replace(Some(Box::new(on_apply)));

        // Load local contacts
        let store = ContactStoreService::new();
        let loaded = store
            .load_contacts()
            .into_iter()
            .map(|record| record.contact)
            .collect::<Vec<_>>();
        dialog.imp().contacts.replace(loaded);

        dialog.build_ui();
        dialog
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let cancel_button = gtk::Button::builder()
            .label("Abbrechen")
            .css_classes(vec!["flat"])
            .build();
        let apply_button = gtk::Button::builder()
            .label("Übernehmen")
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&cancel_button);
        header_bar.pack_end(&apply_button);
        toolbar_view.add_top_bar(&header_bar);

        let dialog_weak = self.downgrade();
        cancel_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
        });

        let dialog_weak = self.downgrade();
        apply_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                let to = format_destinations(&dialog.imp().destinations_to.borrow());
                let cc = format_destinations(&dialog.imp().destinations_cc.borrow());
                let bcc = format_destinations(&dialog.imp().destinations_bcc.borrow());
                if let Some(cb) = dialog.imp().callback.borrow_mut().take() {
                    cb(to, cc, bcc);
                }
                dialog.close();
            }
        });

        // Main content box
        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(16)
            .build();

        // Search bar
        let search_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let search_label = gtk::Label::builder()
            .label("Suchen:")
            .css_classes(vec!["dim-label"])
            .build();
        let search_entry = gtk::SearchEntry::builder()
            .hexpand(true)
            .placeholder_text("Kontakte nach Name oder E-Mail filtern…")
            .build();
        search_box.append(&search_label);
        search_box.append(&search_entry);
        content_box.append(&search_box);

        // Paned / Grid for 3 sections: Contacts List | Transfer Buttons | Destination Lists
        let main_grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(8)
            .hexpand(true)
            .vexpand(true)
            .build();

        // --- Left: Contacts List ---
        let contacts_frame = gtk::Frame::builder()
            .hexpand(true)
            .vexpand(true)
            .build();
        let contacts_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        let contacts_header = gtk::Label::builder()
            .label("<b>Kontakte</b>")
            .use_markup(true)
            .xalign(0.0)
            .margin_start(8)
            .margin_top(6)
            .margin_bottom(4)
            .build();
        contacts_box.append(&contacts_header);

        let contacts_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["boxed-list"])
            .build();
        let contacts_scroll = gtk::ScrolledWindow::builder()
            .child(&contacts_list)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        contacts_box.append(&contacts_scroll);
        contacts_frame.set_child(Some(&contacts_box));
        main_grid.attach(&contacts_frame, 0, 0, 1, 1);

        // --- Middle: Transfer Buttons ---
        let button_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .valign(gtk::Align::Center)
            .build();

        let btn_to = gtk::Button::builder()
            .label("An ->")
            .tooltip_text("Ausgewählten Kontakt zu 'An' hinzufügen")
            .build();
        let btn_cc = gtk::Button::builder()
            .label("Cc ->")
            .tooltip_text("Ausgewählten Kontakt zu 'Cc' hinzufügen")
            .build();
        let btn_bcc = gtk::Button::builder()
            .label("Bcc ->")
            .tooltip_text("Ausgewählten Kontakt zu 'Bcc' hinzufügen")
            .build();

        button_box.append(&btn_to);
        button_box.append(&btn_cc);
        button_box.append(&btn_bcc);
        main_grid.attach(&button_box, 1, 0, 1, 1);

        // --- Right: Destination Lists ---
        let dest_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .vexpand(true)
            .build();

        let (to_container, list_to) = Self::create_destination_group("An:");
        let (cc_container, list_cc) = Self::create_destination_group("Cc:");
        let (bcc_container, list_bcc) = Self::create_destination_group("Bcc:");

        dest_box.append(&to_container);
        dest_box.append(&cc_container);
        dest_box.append(&bcc_container);
        main_grid.attach(&dest_box, 2, 0, 1, 1);

        content_box.append(&main_grid);
        toolbar_view.set_content(Some(&content_box));
        self.set_child(Some(&toolbar_view));

        // Populate initial destination lists
        Self::refresh_destination_list(self, &list_to, 0);
        Self::refresh_destination_list(self, &list_cc, 1);
        Self::refresh_destination_list(self, &list_bcc, 2);

        // Populate contacts list
        let contacts_cache = Rc::new(RefCell::new(self.imp().contacts.borrow().clone()));
        let filtered_contacts = Rc::new(RefCell::new(contacts_cache.borrow().clone()));

        Self::populate_contacts_list(&contacts_list, &filtered_contacts.borrow());

        // Connect Search
        let contacts_list_clone = contacts_list.clone();
        let contacts_cache_clone = Rc::clone(&contacts_cache);
        let filtered_contacts_clone = Rc::clone(&filtered_contacts);
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string();
            let matches = filter_contacts(&contacts_cache_clone.borrow(), &query)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            filtered_contacts_clone.replace(matches);
            Self::populate_contacts_list(&contacts_list_clone, &filtered_contacts_clone.borrow());
        });

        // Connect Transfer Buttons
        let wire_transfer = |button: &gtk::Button,
                             dialog_weak: glib::WeakRef<NameSelectorDialog>,
                             filtered: Rc<RefCell<Vec<VCardContact>>>,
                             contacts_list: gtk::ListBox,
                             dest_list: gtk::ListBox,
                             dest_type: usize| {
            button.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                let Some(selected_row) = contacts_list.selected_row() else {
                    return;
                };
                let index = selected_row.index();
                if index < 0 {
                    return;
                }
                let filtered_borrow = filtered.borrow();
                let Some(contact) = filtered_borrow.get(index as usize) else {
                    return;
                };

                let email = if !contact.email.is_empty() {
                    contact.email.clone()
                } else if let Some(first_email) = contact.emails.first() {
                    first_email.clone()
                } else {
                    String::new()
                };

                let dest = NameSelectorDestination::new(&contact.fn_name, email);
                match dest_type {
                    0 => {
                        let mut list = dialog.imp().destinations_to.borrow_mut();
                        if !list.iter().any(|d| d.email == dest.email && !d.email.is_empty()) {
                            list.push(dest);
                        }
                    }
                    1 => {
                        let mut list = dialog.imp().destinations_cc.borrow_mut();
                        if !list.iter().any(|d| d.email == dest.email && !d.email.is_empty()) {
                            list.push(dest);
                        }
                    }
                    2 => {
                        let mut list = dialog.imp().destinations_bcc.borrow_mut();
                        if !list.iter().any(|d| d.email == dest.email && !d.email.is_empty()) {
                            list.push(dest);
                        }
                    }
                    _ => {}
                }
                Self::refresh_destination_list(&dialog, &dest_list, dest_type);
            });
        };

        wire_transfer(
            &btn_to,
            self.downgrade(),
            Rc::clone(&filtered_contacts),
            contacts_list.clone(),
            list_to.clone(),
            0,
        );
        wire_transfer(
            &btn_cc,
            self.downgrade(),
            Rc::clone(&filtered_contacts),
            contacts_list.clone(),
            list_cc.clone(),
            1,
        );
        wire_transfer(
            &btn_bcc,
            self.downgrade(),
            Rc::clone(&filtered_contacts),
            contacts_list.clone(),
            list_bcc.clone(),
            2,
        );

        // Double click on contact transfers to "An"
        let btn_to_clone = btn_to.clone();
        contacts_list.connect_row_activated(move |_, _| {
            btn_to_clone.emit_clicked();
        });
    }

    fn create_destination_group(title: &str) -> (gtk::Box, gtk::ListBox) {
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .vexpand(true)
            .build();

        let header = gtk::Label::builder()
            .label(&format!("<b>{title}</b>"))
            .use_markup(true)
            .xalign(0.0)
            .build();
        container.append(&header);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(vec!["boxed-list"])
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hexpand(true)
            .vexpand(true)
            .min_content_height(80)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        container.append(&scroll);

        (container, list)
    }

    fn populate_contacts_list(list_box: &gtk::ListBox, contacts: &[VCardContact]) {
        while let Some(row) = list_box.first_child() {
            list_box.remove(&row);
        }
        for contact in contacts {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_start(8)
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();

            let icon = gtk::Image::builder()
                .icon_name(if contact.is_list {
                    "system-users-symbolic"
                } else {
                    "avatar-default-symbolic"
                })
                .build();
            row.append(&icon);

            let label_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(1)
                .hexpand(true)
                .build();

            let name_label = gtk::Label::builder()
                .label(if !contact.fn_name.is_empty() {
                    &contact.fn_name
                } else if !contact.list_name.is_empty() {
                    &contact.list_name
                } else {
                    "(Ohne Name)"
                })
                .xalign(0.0)
                .build();
            label_box.append(&name_label);

            let email_str = if !contact.email.is_empty() {
                contact.email.clone()
            } else if let Some(first) = contact.emails.first() {
                first.clone()
            } else if contact.is_list {
                format!("{} Mitglieder", contact.list_members.len())
            } else {
                String::new()
            };

            if !email_str.is_empty() {
                let email_label = gtk::Label::builder()
                    .label(&email_str)
                    .xalign(0.0)
                    .css_classes(vec!["dim-label", "caption"])
                    .build();
                label_box.append(&email_label);
            }

            row.append(&label_box);
            list_box.append(&row);
        }
    }

    fn refresh_destination_list(dialog: &NameSelectorDialog, list_box: &gtk::ListBox, dest_type: usize) {
        while let Some(row) = list_box.first_child() {
            list_box.remove(&row);
        }

        let destinations = match dest_type {
            0 => dialog.imp().destinations_to.borrow().clone(),
            1 => dialog.imp().destinations_cc.borrow().clone(),
            2 => dialog.imp().destinations_bcc.borrow().clone(),
            _ => Vec::new(),
        };

        for (idx, dest) in destinations.iter().enumerate() {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .margin_start(6)
                .margin_end(6)
                .margin_top(2)
                .margin_bottom(2)
                .build();

            let label = gtk::Label::builder()
                .label(&dest.formatted())
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            row.append(&label);

            let remove_btn = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .css_classes(vec!["flat", "circular"])
                .tooltip_text("Empfänger entfernen")
                .build();

            let dialog_weak = dialog.downgrade();
            let list_box_clone = list_box.clone();
            remove_btn.connect_clicked(move |_| {
                let Some(dialog) = dialog_weak.upgrade() else {
                    return;
                };
                match dest_type {
                    0 => {
                        let mut list = dialog.imp().destinations_to.borrow_mut();
                        if idx < list.len() {
                            list.remove(idx);
                        }
                    }
                    1 => {
                        let mut list = dialog.imp().destinations_cc.borrow_mut();
                        if idx < list.len() {
                            list.remove(idx);
                        }
                    }
                    2 => {
                        let mut list = dialog.imp().destinations_bcc.borrow_mut();
                        if idx < list.len() {
                            list.remove(idx);
                        }
                    }
                    _ => {}
                }
                Self::refresh_destination_list(&dialog, &list_box_clone, dest_type);
            });

            row.append(&remove_btn);
            list_box.append(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_destinations_extracts_tokens_and_names() {
        let text = "Ada Lovelace <ada@example.org>, bob@example.org; \"Grace Hopper\" <grace@navy.mil>";
        let parsed = parse_destinations(text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].display_name, "Ada Lovelace");
        assert_eq!(parsed[0].email, "ada@example.org");
        assert_eq!(parsed[1].display_name, "");
        assert_eq!(parsed[1].email, "bob@example.org");
        assert_eq!(parsed[2].display_name, "Grace Hopper");
        assert_eq!(parsed[2].email, "grace@navy.mil");
    }

    #[test]
    fn format_destinations_builds_comma_separated_string() {
        let dests = vec![
            NameSelectorDestination::new("Ada Lovelace", "ada@example.org"),
            NameSelectorDestination::new("", "bob@example.org"),
        ];
        assert_eq!(
            format_destinations(&dests),
            "Ada Lovelace <ada@example.org>, bob@example.org"
        );
    }

    #[test]
    fn filter_contacts_matches_name_email_and_nickname() {
        let mut c1 = VCardContact::new(
            "Linus Torvalds",
            "torvalds@linux-foundation.org",
            "Linux Foundation",
            "",
        );
        c1.nickname = "linus".to_string();

        let mut c2 = VCardContact::new("Richard Stallman", "rms@gnu.org", "FSF", "");
        c2.nickname = "rms".to_string();

        let contacts = vec![c1, c2];

        assert_eq!(filter_contacts(&contacts, "linus").len(), 1);
        assert_eq!(filter_contacts(&contacts, "gnu.org").len(), 1);
        assert_eq!(filter_contacts(&contacts, "fsf").len(), 1);
        assert_eq!(filter_contacts(&contacts, "").len(), 2);
        assert_eq!(filter_contacts(&contacts, "nobody").len(), 0);
    }
}
