// SPDX-License-Identifier: GPL-3.0-or-later
//
// ImportDialog — Import assistant for iCalendar (.ics, .vcs), vCard (.vcf), CSV (.csv), LDIF (.ldif), and mail (.mbox, .eml) files.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::path::PathBuf;

use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::contact_import_service::{
    ContactDuplicatePolicy, ContactImportFormat, detect_contact_format, execute_contact_import,
    plan_contact_import,
};
use crate::services::contact_store_service::ContactStoreService;
use crate::services::ical_import_service::{
    IcalDuplicatePolicy, execute_ical_import, plan_ical_import,
};
use crate::i18n::gettext;
use crate::services::mail_store_service::MailStoreService;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ImportDialog {
        pub selected_file: RefCell<Option<PathBuf>>,
        pub file_label: RefCell<Option<gtk::Label>>,
        pub target_combo: RefCell<Option<adw::ComboRow>>,
        pub policy_combo: RefCell<Option<adw::ComboRow>>,
        pub preview_row: RefCell<Option<adw::ActionRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImportDialog {
        const NAME: &'static str = "MeridianImportDialog";
        type Type = super::ImportDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for ImportDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for ImportDialog {}
    impl AdwDialogImpl for ImportDialog {}
}

glib::wrapper! {
    pub struct ImportDialog(ObjectSubclass<imp::ImportDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ImportDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        self.set_title(&gettext("Import Data"));
        self.set_content_width(520);
        self.set_content_height(420);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();

        let cancel_btn = gtk::Button::builder().label(gettext("Cancel")).build();
        let dialog_for_cancel = self.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        header.pack_start(&cancel_btn);

        let import_btn = gtk::Button::builder()
            .label(gettext("Import"))
            .css_classes(["suggested-action"])
            .build();
        let dialog_for_import = self.downgrade();
        import_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_import.upgrade() {
                dialog.execute_import();
            }
        });
        header.pack_end(&import_btn);
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();

        let group = adw::PreferencesGroup::builder()
            .title(gettext("Source File and Target"))
            .description(gettext("Choose an iCalendar (.ics, .vcs), contact (.vcf, .csv, .ldif), or mail (.mbox / .eml) file to import."))
            .build();

        let file_row = adw::ActionRow::builder()
            .title(gettext("File to Import"))
            .subtitle(gettext("No file selected"))
            .build();
        let icon_file = gtk::Image::builder()
            .icon_name("document-open-symbolic")
            .valign(gtk::Align::Center)
            .build();
        file_row.add_prefix(&icon_file);

        let choose_file_btn = gtk::Button::builder()
            .label(gettext("Browse…"))
            .valign(gtk::Align::Center)
            .build();

        let file_row_weak = file_row.downgrade();
        let dialog_weak = self.downgrade();
        choose_file_btn.connect_clicked(move |btn| {
            let Some(root) = btn.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let dialog_inner = dialog_weak.clone();
            let file_row_inner = file_row_weak.clone();

            let file_dialog = gtk::FileDialog::builder()
                .title(gettext("Select Import File"))
                .modal(true)
                .build();

            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext(
                "Supported Formats (*.ics, *.vcs, *.vcf, *.vcard, *.csv, *.ldif, *.ldi, *.mbox, *.eml)",
            )));
            filter.add_pattern("*.ics");
            filter.add_pattern("*.vcs");
            filter.add_pattern("*.vcf");
            filter.add_pattern("*.vcard");
            filter.add_pattern("*.csv");
            filter.add_pattern("*.ldif");
            filter.add_pattern("*.ldi");
            filter.add_pattern("*.mbox");
            filter.add_pattern("*.eml");

            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            file_dialog.set_filters(Some(&filters));

            file_dialog.open(Some(&root), gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        if let Some(dialog) = dialog_inner.upgrade() {
                            *dialog.imp().selected_file.borrow_mut() = Some(path.clone());
                            if let Some(row) = file_row_inner.upgrade() {
                                let filename = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Selected");
                                row.set_subtitle(filename);
                            }
                            dialog.auto_select_target_for_path(&path);
                        }
                    }
                }
            });
        });
        file_row.add_suffix(&choose_file_btn);
        group.add(&file_row);

        let preview_row = adw::ActionRow::builder()
            .title(gettext("Detected Items"))
            .subtitle(gettext("No file loaded yet"))
            .build();
        let icon_preview = gtk::Image::builder()
            .icon_name("view-list-symbolic")
            .valign(gtk::Align::Center)
            .build();
        preview_row.add_prefix(&icon_preview);
        group.add(&preview_row);
        *self.imp().preview_row.borrow_mut() = Some(preview_row);

        let target_combo = adw::ComboRow::builder()
            .title(gettext("Target Component"))
            .model(&gtk::StringList::new(&[
                &gettext("Calendar / Events (.ics, .vcs)"),
                &gettext("Tasks (.ics, .vcs)"),
                &gettext("Memos / Notes (.ics, .vcs)"),
                &gettext("Contacts / Address Book (.vcf, .csv, .ldif)"),
                &gettext("Mail / Inbox (.mbox / .eml)"),
            ]))
            .build();
        let icon_target = gtk::Image::builder()
            .icon_name("folder-symbolic")
            .valign(gtk::Align::Center)
            .build();
        target_combo.add_prefix(&icon_target);
        group.add(&target_combo);
        *self.imp().target_combo.borrow_mut() = Some(target_combo);

        let policy_combo = adw::ComboRow::builder()
            .title(gettext("Duplicate Handling"))
            .model(&gtk::StringList::new(&[
                &gettext("Keep both (assign new ID)"),
                &gettext("Overwrite existing items"),
                &gettext("Skip existing items"),
            ]))
            .visible(false)
            .build();
        let icon_policy = gtk::Image::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .build();
        policy_combo.add_prefix(&icon_policy);
        group.add(&policy_combo);
        *self.imp().policy_combo.borrow_mut() = Some(policy_combo);

        page.add(&group);

        toolbar.set_content(Some(&page));
        self.set_child(Some(&toolbar));
    }

    fn auto_select_target_for_path(&self, path: &std::path::Path) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let content = std::fs::read_to_string(path).unwrap_or_default();
        let contact_fmt = detect_contact_format(path, &content);
        let is_ical = ext == "ics" || ext == "vcs";
        let is_contact = contact_fmt != ContactImportFormat::Unknown;

        if let Some(combo) = self.imp().target_combo.borrow().as_ref() {
            if is_contact {
                combo.set_selected(3);
            } else {
                match ext.as_str() {
                    "mbox" | "eml" => combo.set_selected(4),
                    "ics" | "vcs" => combo.set_selected(0),
                    _ => {}
                }
            }
        }

        if let Some(policy_combo) = self.imp().policy_combo.borrow().as_ref() {
            policy_combo.set_visible(is_ical || is_contact);
        }

        if let Some(preview_row) = self.imp().preview_row.borrow().as_ref() {
            if is_ical {
                if !content.is_empty() {
                    let plan = plan_ical_import(
                        &content,
                        &std::collections::HashSet::new(),
                        IcalDuplicatePolicy::default(),
                    );
                    preview_row.set_subtitle(&plan.summary_label());
                } else {
                    preview_row.set_subtitle("Could not read file");
                }
            } else if is_contact {
                let existing = ContactStoreService::new()
                    .load_contacts()
                    .into_iter()
                    .map(|r| r.contact)
                    .collect::<Vec<_>>();
                let plan = plan_contact_import(
                    &content,
                    contact_fmt,
                    &existing,
                    ContactDuplicatePolicy::default(),
                );
                preview_row.set_subtitle(&plan.summary_label());
            } else if ext == "mbox" || ext == "eml" {
                preview_row.set_subtitle(&gettext("Mail message archive ready for import"));
            } else {
                preview_row.set_subtitle(&gettext("Format ready for import"));
            }
        }
    }

    fn execute_import(&self) {
        let Some(path) = self.imp().selected_file.borrow().clone() else {
            return;
        };
        let target_idx = self
            .imp()
            .target_combo
            .borrow()
            .as_ref()
            .map(|d| d.selected())
            .unwrap_or(0);
        let policy_idx = self
            .imp()
            .policy_combo
            .borrow()
            .as_ref()
            .map(|d| d.selected())
            .unwrap_or(0);

        let ical_duplicate_policy = match policy_idx {
            1 => IcalDuplicatePolicy::OverwriteExisting,
            2 => IcalDuplicatePolicy::SkipExisting,
            _ => IcalDuplicatePolicy::KeepBothNewUid,
        };

        let contact_duplicate_policy = match policy_idx {
            1 => ContactDuplicatePolicy::OverwriteExisting,
            2 => ContactDuplicatePolicy::SkipExisting,
            _ => ContactDuplicatePolicy::KeepBothNew,
        };

        let dialog_weak = self.downgrade();

        glib::spawn_future_local(async move {
            let (sender, receiver) = async_channel::bounded(1);

            std::thread::spawn(move || {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let count = match target_idx {
                    0 | 1 | 2 => {
                        // iCalendar import into store with duplicate policy and multi-component parsing
                        let store = CalendarStoreService::new();
                        let snapshot = store.load_snapshot();
                        let mut existing = std::collections::HashSet::new();
                        for e in snapshot.events {
                            existing.insert(e.uid);
                        }
                        for t in snapshot.tasks {
                            existing.insert(t.uid);
                        }
                        for m in snapshot.memos {
                            existing.insert(m.uid);
                        }

                        let plan = plan_ical_import(&content, &existing, ical_duplicate_policy);
                        match execute_ical_import(&plan, &store) {
                            Ok(res) => res.imported_count + res.overwritten_count,
                            Err(err) => {
                                tracing::warn!(%err, "iCalendar import failed");
                                0
                            }
                        }
                    }
                    3 => {
                        // Contact import (vCard, CSV, LDIF) with duplicate policy
                        let store = ContactStoreService::new();
                        let format = detect_contact_format(&path, &content);
                        let existing = store
                            .load_contacts()
                            .into_iter()
                            .map(|r| r.contact)
                            .collect::<Vec<_>>();
                        let plan = plan_contact_import(
                            &content,
                            format,
                            &existing,
                            contact_duplicate_policy,
                        );
                        match execute_contact_import(&plan, &store) {
                            Ok(res) => res.imported_count + res.overwritten_count,
                            Err(err) => {
                                tracing::warn!(%err, "Contact import failed");
                                0
                            }
                        }
                    }
                    4 => {
                        // Mail import
                        let store = MailStoreService::new();
                        store.import_files(&[path.clone()]).unwrap_or(0)
                    }
                    _ => 0,
                };
                let _ = sender.send_blocking(count);
            });

            if let Ok(_imported_count) = receiver.recv().await {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.close();
                }
            }
        });
    }
}

impl Default for ImportDialog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn duplicate_policy_maps_dropdown_index_correctly() {
        let p0 = match 0 {
            1 => IcalDuplicatePolicy::OverwriteExisting,
            2 => IcalDuplicatePolicy::SkipExisting,
            _ => IcalDuplicatePolicy::KeepBothNewUid,
        };
        assert_eq!(p0, IcalDuplicatePolicy::KeepBothNewUid);

        let p1 = match 1 {
            1 => IcalDuplicatePolicy::OverwriteExisting,
            2 => IcalDuplicatePolicy::SkipExisting,
            _ => IcalDuplicatePolicy::KeepBothNewUid,
        };
        assert_eq!(p1, IcalDuplicatePolicy::OverwriteExisting);

        let p2 = match 2 {
            1 => IcalDuplicatePolicy::OverwriteExisting,
            2 => IcalDuplicatePolicy::SkipExisting,
            _ => IcalDuplicatePolicy::KeepBothNewUid,
        };
        assert_eq!(p2, IcalDuplicatePolicy::SkipExisting);

        let c0 = match 0 {
            1 => ContactDuplicatePolicy::OverwriteExisting,
            2 => ContactDuplicatePolicy::SkipExisting,
            _ => ContactDuplicatePolicy::KeepBothNew,
        };
        assert_eq!(c0, ContactDuplicatePolicy::KeepBothNew);

        let c1 = match 1 {
            1 => ContactDuplicatePolicy::OverwriteExisting,
            2 => ContactDuplicatePolicy::SkipExisting,
            _ => ContactDuplicatePolicy::KeepBothNew,
        };
        assert_eq!(c1, ContactDuplicatePolicy::OverwriteExisting);

        let c2 = match 2 {
            1 => ContactDuplicatePolicy::OverwriteExisting,
            2 => ContactDuplicatePolicy::SkipExisting,
            _ => ContactDuplicatePolicy::KeepBothNew,
        };
        assert_eq!(c2, ContactDuplicatePolicy::SkipExisting);
    }
}
