// SPDX-License-Identifier: GPL-3.0-or-later
//
// ImportDialog — Import assistant for iCalendar (.ics), vCard (.vcf), and mail (.mbox, .eml) files.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::path::PathBuf;

use crate::services::calendar_store_service::CalendarStoreService;
use crate::services::contact_store_service::ContactStoreService;
use crate::services::mail_store_service::MailStoreService;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct ImportDialog {
        pub selected_file: RefCell<Option<PathBuf>>,
        pub file_label: RefCell<Option<gtk::Label>>,
        pub target_dropdown: RefCell<Option<gtk::DropDown>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImportDialog {
        const NAME: &'static str = "EvolutionImportDialog";
        type Type = super::ImportDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for ImportDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for ImportDialog {}
    impl WindowImpl for ImportDialog {}
    impl AdwWindowImpl for ImportDialog {}
}

glib::wrapper! {
    pub struct ImportDialog(ObjectSubclass<imp::ImportDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ImportDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    fn build_ui(&self) {
        self.set_title(Some("Daten importieren"));
        self.set_default_size(520, 420);
        self.set_modal(true);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);

        let main_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        let icon = gtk::Image::builder()
            .icon_name("document-open-symbolic")
            .pixel_size(48)
            .css_classes(vec!["accent"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&icon);

        let title = gtk::Label::builder()
            .label("Evolution Import-Assistent")
            .css_classes(vec!["title-2"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&title);

        let desc = gtk::Label::builder()
            .label("Wählen Sie eine iCalendar- (.ics), vCard- (.vcf) oder Mail-Datei (.mbox / .eml), um Daten in Ihr lokales Adressbuch, Ihren Kalender oder Ihre Mailbox zu importieren.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Center)
            .build();
        main_box.append(&desc);

        let group = adw::PreferencesGroup::builder()
            .title("Quelldatei und Ziel")
            .build();

        let file_row = adw::ActionRow::builder()
            .title("Zu importierende Datei")
            .subtitle("Keine Datei ausgewählt")
            .build();

        let choose_file_btn = gtk::Button::builder()
            .label("Durchsuchen …")
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
                .title("Importdatei auswählen")
                .modal(true)
                .build();

            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Unterstützte Formate (*.ics, *.vcf, *.mbox, *.eml)"));
            filter.add_pattern("*.ics");
            filter.add_pattern("*.vcf");
            filter.add_pattern("*.vcard");
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
                                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("Ausgewählt");
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

        let target_dropdown = gtk::DropDown::from_strings(&[
            "Kalender / Termine (.ics)",
            "Aufgaben / Tasks (.ics)",
            "Memos / Notizen (.ics)",
            "Kontakte / Adressbuch (.vcf)",
            "E-Mail / Posteingang (.mbox / .eml)",
        ]);
        target_dropdown.set_valign(gtk::Align::Center);
        let target_row = adw::ActionRow::builder()
            .title("Zielkomponente")
            .build();
        target_row.add_suffix(&target_dropdown);
        group.add(&target_row);
        *self.imp().target_dropdown.borrow_mut() = Some(target_dropdown);

        main_box.append(&group);

        // Buttons
        let button_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::End)
            .margin_top(8)
            .build();

        let cancel_btn = gtk::Button::builder()
            .label("Abbrechen")
            .build();
        let dialog_for_cancel = self.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_cancel.upgrade() {
                dialog.close();
            }
        });
        button_box.append(&cancel_btn);

        let import_btn = gtk::Button::builder()
            .label("Importieren")
            .css_classes(vec!["suggested-action"])
            .build();
        let dialog_for_import = self.downgrade();
        import_btn.connect_clicked(move |_| {
            if let Some(dialog) = dialog_for_import.upgrade() {
                dialog.execute_import();
            }
        });
        button_box.append(&import_btn);
        main_box.append(&button_box);

        toolbar.set_content(Some(&main_box));
        self.set_content(Some(&toolbar));
    }

    fn auto_select_target_for_path(&self, path: &std::path::Path) {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if let Some(dropdown) = self.imp().target_dropdown.borrow().as_ref() {
            match ext.as_str() {
                "vcf" | "vcard" => dropdown.set_selected(3),
                "mbox" | "eml" => dropdown.set_selected(4),
                "ics" => dropdown.set_selected(0),
                _ => {}
            }
        }
    }

    fn execute_import(&self) {
        let Some(path) = self.imp().selected_file.borrow().clone() else {
            return;
        };
        let target_idx = self.imp().target_dropdown.borrow().as_ref().map(|d| d.selected()).unwrap_or(0);
        let dialog_weak = self.downgrade();

        glib::spawn_future_local(async move {
            let (sender, receiver) = async_channel::bounded(1);

            std::thread::spawn(move || {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                let count = match target_idx {
                    0 | 1 | 2 => {
                        // iCalendar import into local store
                        let store = CalendarStoreService::new();
                        let mut imported = 0usize;
                        if !content.is_empty() {
                            if store.create_component(&content).is_ok() {
                                imported += 1;
                            }
                        }
                        imported
                    }
                    3 => {
                        // vCard contact import
                        let store = ContactStoreService::new();
                        let mut imported = 0usize;
                        if !content.is_empty() {
                            if store.import_vcard_content(&content).is_ok() {
                                imported += 1;
                            }
                        }
                        imported
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
