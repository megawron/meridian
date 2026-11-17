// SPDX-License-Identifier: GPL-3.0-or-later
//
// ExtensionsDialog — Manage built-in optional add-ons and user-installed
// folder extensions (VS Code / mod-style contribution points).

use adw::prelude::*;
use gtk::gio;
use std::cell::RefCell;
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::extension_service::{ExtensionRegistry, InstalledExtension};

pub struct ExtensionsDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl ExtensionsDialog {
    pub fn new<P: IsA<gtk::Widget>>(parent: Option<&P>) -> Self {
        let dialog = adw::Dialog::builder()
            .title(gettext("Extensions"))
            .content_width(640)
            .content_height(560)
            .follows_content_size(false)
            .build();

        let stored_parent = RefCell::new(parent.map(|p| p.upcast_ref::<gtk::Widget>().clone()));

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar_view.add_top_bar(&header);

        let search_clamp = adw::Clamp::builder()
            .maximum_size(600)
            .margin_start(16)
            .margin_end(16)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search extensions…"))
            .hexpand(true)
            .build();
        search_clamp.set_child(Some(&search_entry));
        toolbar_view.add_top_bar(&search_clamp);

        let toast_overlay = adw::ToastOverlay::new();
        let page = adw::PreferencesPage::new();

        let group_bundled = adw::PreferencesGroup::builder()
            .title(gettext("Optional Features"))
            .build();
        let group_user = adw::PreferencesGroup::builder()
            .title(gettext("Installed Extensions"))
            .build();
        let group_tools = adw::PreferencesGroup::builder()
            .title(gettext("Manage"))
            .build();

        let searchable_rows: Rc<RefCell<Vec<(String, gtk::Widget)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let user_rows_tracker: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));

        let rebuild_lists: Rc<dyn Fn(&str)> = {
            let group_bundled = group_bundled.clone();
            let group_user = group_user.clone();
            let searchable_rows = Rc::clone(&searchable_rows);
            let user_rows_tracker = Rc::clone(&user_rows_tracker);
            let toast_overlay_ref = toast_overlay.clone();
            let dialog_weak = dialog.downgrade();

            let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>> =
                Rc::new(RefCell::new(None));
            let rebuild_self = Rc::clone(&rebuild_holder);

            let closure: Rc<dyn Fn(&str)> = Rc::new(move |filter_query: &str| {
                for (_, w) in searchable_rows.borrow_mut().drain(..) {
                    if let Some(p) = w.parent() {
                        if p == group_bundled.clone().upcast::<gtk::Widget>() {
                            group_bundled.remove(&w);
                        }
                    }
                }
                for w in user_rows_tracker.borrow_mut().drain(..) {
                    group_user.remove(&w);
                }

                let reg = ExtensionRegistry::default();
                let all = reg.list_all();
                let q = filter_query.trim().to_ascii_lowercase();

                let mut bundled_count = 0usize;
                let mut user_count = 0usize;

                for ext in all {
                    let hay = format!(
                        "{} {} {}",
                        ext.manifest.name.to_ascii_lowercase(),
                        ext.manifest.description.to_ascii_lowercase(),
                        ext.manifest.id.to_ascii_lowercase()
                    );
                    if !q.is_empty() && !hay.contains(&q) {
                        continue;
                    }
                    if ext.is_bundled {
                        let row = build_extension_switch_row(&ext, &toast_overlay_ref);
                        group_bundled.add(&row);
                        searchable_rows
                            .borrow_mut()
                            .push((hay, row.upcast::<gtk::Widget>()));
                        bundled_count += 1;
                    } else {
                        let row = build_extension_switch_row(&ext, &toast_overlay_ref);
                        let remove_btn = gtk::Button::builder()
                            .icon_name("user-trash-symbolic")
                            .tooltip_text(gettext("Remove extension"))
                            .css_classes(["flat", "error"])
                            .valign(gtk::Align::Center)
                            .build();
                        let ext_id = ext.manifest.id.clone();
                        let toast_for_rm = toast_overlay_ref.clone();
                        let rebuild_after_rm = Rc::clone(&rebuild_self);
                        let dlg_for_rm = dialog_weak.clone();
                        remove_btn.connect_clicked(move |_| {
                            let Some(dlg) = dlg_for_rm.upgrade() else {
                                return;
                            };
                            let id_owned = ext_id.clone();
                            let toast_c = toast_for_rm.clone();
                            let rebuild_c = Rc::clone(&rebuild_after_rm);
                            crate::dialogs::run_destructive_action(
                                &dlg,
                                &gettext("Remove Extension?"),
                                &gettext("The extension folder will be removed from local storage."),
                                &gettext("Remove"),
                                move || {
                                    match ExtensionRegistry::default().uninstall(&id_owned) {
                                        Ok(()) => {
                                            toast_c.add_toast(adw::Toast::new(&gettext(
                                                "Extension removed",
                                            )));
                                            if let Some(f) = rebuild_c.borrow().as_ref() {
                                                f("");
                                            }
                                        }
                                        Err(err) => {
                                            toast_c.add_toast(adw::Toast::new(&err));
                                        }
                                    }
                                },
                            );
                        });
                        row.add_suffix(&remove_btn);
                        group_user.add(&row);
                        user_rows_tracker
                            .borrow_mut()
                            .push(row.upcast::<gtk::Widget>());
                        user_count += 1;
                    }
                }

                if user_count == 0 && q.is_empty() {
                    let empty_row = adw::ActionRow::builder()
                        .title(gettext("No custom extensions installed"))
                        .build();
                    group_user.add(&empty_row);
                    user_rows_tracker
                        .borrow_mut()
                        .push(empty_row.upcast::<gtk::Widget>());
                }

                group_bundled.set_visible(bundled_count > 0 || q.is_empty());
                group_user.set_visible(user_count > 0 || q.is_empty());
            });

            *rebuild_holder.borrow_mut() = Some(Rc::clone(&closure));
            closure
        };

        rebuild_lists("");

        let rebuild_for_search = Rc::clone(&rebuild_lists);
        search_entry.connect_search_changed(move |entry| {
            rebuild_for_search(&entry.text());
        });

        // Install from Folder…
        let row_install = adw::ActionRow::builder()
            .title(gettext("Install Extension from Folder…"))
            .activatable(true)
            .build();
        {
            let dialog_weak = dialog.downgrade();
            let toast_c = toast_overlay.clone();
            let rebuild_c = Rc::clone(&rebuild_lists);
            row_install.connect_activated(move |_| {
                let Some(dlg) = dialog_weak.upgrade() else {
                    return;
                };
                let win = dlg.root().and_downcast::<gtk::Window>();
                let file_dialog = gtk::FileDialog::builder()
                    .title(gettext("Select Extension Folder (containing extension.json)"))
                    .modal(true)
                    .build();
                let toast_inner = toast_c.clone();
                let rebuild_inner = Rc::clone(&rebuild_c);
                file_dialog.select_folder(
                    win.as_ref(),
                    gio::Cancellable::NONE,
                    move |result| {
                        let Ok(folder) = result else {
                            return;
                        };
                        let Some(path) = folder.path() else {
                            return;
                        };
                        match ExtensionRegistry::default().install_from_directory(&path) {
                            Ok(manifest) => {
                                toast_inner.add_toast(adw::Toast::new(&format!(
                                    "{}: {}",
                                    gettext("Installed extension"),
                                    manifest.name
                                )));
                                rebuild_inner("");
                            }
                            Err(err) => {
                                toast_inner.add_toast(adw::Toast::new(&err));
                            }
                        }
                    },
                );
            });
        }
        group_tools.add(&row_install);

        // Create Starter Extension…
        let row_scaffold = adw::ActionRow::builder()
            .title(gettext("Create Starter Extension…"))
            .activatable(true)
            .build();
        {
            let dialog_weak = dialog.downgrade();
            let toast_c = toast_overlay.clone();
            let rebuild_c = Rc::clone(&rebuild_lists);
            row_scaffold.connect_activated(move |_| {
                let Some(dlg) = dialog_weak.upgrade() else {
                    return;
                };
                present_starter_extension_prompt(&dlg, &toast_c, Rc::clone(&rebuild_c));
            });
        }
        group_tools.add(&row_scaffold);

        // Open Extensions Folder
        let row_open_dir = adw::ActionRow::builder()
            .title(gettext("Open Extensions Folder"))
            .activatable(true)
            .build();
        row_open_dir.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
        {
            let toast_c = toast_overlay.clone();
            row_open_dir.connect_activated(move |_| {
                let dir = crate::services::extension_service::extensions_dir();
                let _ = std::fs::create_dir_all(&dir);
                let uri = gio::File::for_path(&dir).uri();
                if let Err(err) =
                    gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
                {
                    toast_c.add_toast(adw::Toast::new(&format!(
                        "{}: {err}",
                        gettext("Could not open folder")
                    )));
                }
            });
        }
        group_tools.add(&row_open_dir);

        page.add(&group_bundled);
        page.add(&group_user);
        page.add(&group_tools);

        toast_overlay.set_child(Some(&page));
        toolbar_view.set_content(Some(&toast_overlay));
        dialog.set_child(Some(&toolbar_view));

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
    }
}

pub fn present_extensions_dialog<P: IsA<gtk::Widget>>(parent: Option<&P>) {
    let dialog = ExtensionsDialog::new(parent);
    dialog.present();
}

fn build_extension_switch_row(
    ext: &InstalledExtension,
    toast_overlay: &adw::ToastOverlay,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(&ext.manifest.name)
        .subtitle(&ext.manifest.description)
        .use_markup(false)
        .active(ext.enabled)
        .build();
    let ext_id = ext.manifest.id.clone();
    let toast_c = toast_overlay.clone();
    row.connect_active_notify(move |switch_row| {
        if let Err(err) = ExtensionRegistry::default().set_enabled(&ext_id, switch_row.is_active())
        {
            toast_c.add_toast(adw::Toast::new(&err.to_string()));
        }
    });
    row
}

fn present_starter_extension_prompt(
    parent: &adw::Dialog,
    toast_overlay: &adw::ToastOverlay,
    rebuild: Rc<dyn Fn(&str)>,
) {
    let sheet = adw::Dialog::builder()
        .title(gettext("New Starter Extension"))
        .content_width(440)
        .content_height(260)
        .follows_content_size(false)
        .build();
    let tv = adw::ToolbarView::new();
    let hb = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let btn_cancel = gtk::Button::with_label(&gettext("Cancel"));
    let sheet_w = sheet.downgrade();
    btn_cancel.connect_clicked(move |_| {
        if let Some(s) = sheet_w.upgrade() {
            s.close();
        }
    });
    let btn_create = gtk::Button::builder()
        .label(gettext("Create"))
        .css_classes(["suggested-action"])
        .build();
    hb.pack_start(&btn_cancel);
    hb.pack_end(&btn_create);
    tv.add_top_bar(&hb);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let row_name = adw::EntryRow::builder()
        .title(gettext("Extension Name"))
        .text("My Custom Action")
        .build();
    group.add(&row_name);
    page.add(&group);
    tv.set_content(Some(&page));
    sheet.set_child(Some(&tv));

    let toast_c = toast_overlay.clone();
    let sheet_w2 = sheet.downgrade();
    btn_create.connect_clicked(move |_| {
        match ExtensionRegistry::default().create_starter_extension(&row_name.text()) {
            Ok(folder) => {
                if let Some(s) = sheet_w2.upgrade() {
                    s.close();
                }
                toast_c.add_toast(adw::Toast::new(&format!(
                    "{}: {}",
                    gettext("Created starter extension in"),
                    folder.display()
                )));
                rebuild("");
            }
            Err(err) => {
                toast_c.add_toast(adw::Toast::new(&err));
            }
        }
    });

    sheet.present(Some(parent));
}
