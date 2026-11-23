// SPDX-License-Identifier: GPL-3.0-or-later
//
// ExtensionsDialog — Clean libadwaita manager & Discover browser for Meridian
// optional features and user extensions using adw::NavigationView drill-down pages.

use adw::prelude::*;
use gtk::gio;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::extension_service::{
    CatalogExtensionEntry, ExtensionCatalogService, ExtensionRegistry, ExtensionSettingKind,
    ExtensionViewPlacement, InstalledExtension,
};

pub struct ExtensionsDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl ExtensionsDialog {
    pub fn new<P: IsA<gtk::Widget>>(parent: Option<&P>) -> Self {
        let dialog = adw::Dialog::builder()
            .title(gettext("Extensions"))
            .content_width(560)
            .content_height(560)
            .follows_content_size(false)
            .build();

        let stored_parent = RefCell::new(parent.map(|p| p.upcast_ref::<gtk::Widget>().clone()));

        let toast_overlay = adw::ToastOverlay::new();
        let nav_view = adw::NavigationView::new();

        // ── Root Navigation Page (Installed / Discover) ─────────────────────
        let root_toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let view_stack = adw::ViewStack::new();
        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header.set_title_widget(Some(&view_switcher));
        root_toolbar.add_top_bar(&header);

        let search_clamp = adw::Clamp::builder()
            .maximum_size(520)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search extensions…"))
            .hexpand(true)
            .build();
        search_clamp.set_child(Some(&search_entry));
        root_toolbar.add_top_bar(&search_clamp);

        // Tab 1: Installed
        let installed_page = adw::PreferencesPage::new();
        let group_bundled = adw::PreferencesGroup::builder()
            .title(gettext("Optional Features"))
            .build();
        let group_user = adw::PreferencesGroup::builder()
            .title(gettext("Installed Extensions"))
            .build();
        let group_tools = adw::PreferencesGroup::builder()
            .title(gettext("Manage"))
            .build();

        // Tab 2: Discover
        let discover_page = adw::PreferencesPage::new();
        let group_discover = adw::PreferencesGroup::new();
        discover_page.add(&group_discover);

        let installed_tab =
            view_stack.add_titled(&installed_page, Some("installed"), &gettext("Installed"));
        installed_tab.set_icon_name(Some("application-x-addon-symbolic"));
        let discover_tab =
            view_stack.add_titled(&discover_page, Some("discover"), &gettext("Discover"));
        discover_tab.set_icon_name(Some("find-location-symbolic"));

        root_toolbar.set_content(Some(&view_stack));
        let root_nav_page = adw::NavigationPage::builder()
            .title(gettext("Extensions"))
            .child(&root_toolbar)
            .build();
        nav_view.add(&root_nav_page);

        let bundled_rows_tracker: Rc<RefCell<Vec<gtk::Widget>>> =
            Rc::new(RefCell::new(Vec::new()));
        let user_rows_tracker: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
        let discover_rows_tracker: Rc<RefCell<Vec<gtk::Widget>>> =
            Rc::new(RefCell::new(Vec::new()));
        let catalog_entries: Rc<RefCell<Vec<CatalogExtensionEntry>>> =
            Rc::new(RefCell::new(ExtensionCatalogService::curated_entries()));

        let rebuild_all: Rc<dyn Fn(&str)> = {
            let group_bundled = group_bundled.clone();
            let group_user = group_user.clone();
            let group_discover = group_discover.clone();
            let bundled_rows_tracker = Rc::clone(&bundled_rows_tracker);
            let user_rows_tracker = Rc::clone(&user_rows_tracker);
            let discover_rows_tracker = Rc::clone(&discover_rows_tracker);
            let catalog_entries = Rc::clone(&catalog_entries);
            let toast_overlay_ref = toast_overlay.clone();
            let nav_view_ref = nav_view.clone();
            let dialog_weak = dialog.downgrade();

            let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>> =
                Rc::new(RefCell::new(None));
            let rebuild_self = Rc::clone(&rebuild_holder);

            let closure: Rc<dyn Fn(&str)> = Rc::new(move |filter_query: &str| {
                for w in bundled_rows_tracker.borrow_mut().drain(..) {
                    group_bundled.remove(&w);
                }
                for w in user_rows_tracker.borrow_mut().drain(..) {
                    group_user.remove(&w);
                }
                for w in discover_rows_tracker.borrow_mut().drain(..) {
                    group_discover.remove(&w);
                }

                let reg = ExtensionRegistry::default();
                let all = reg.list_all();
                let installed_ids: HashSet<String> =
                    all.iter().map(|e| e.manifest.id.clone()).collect();
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
                    let row = build_compact_installed_row(
                        &ext,
                        &nav_view_ref,
                        &toast_overlay_ref,
                        &dialog_weak,
                        Rc::clone(&rebuild_self),
                        filter_query,
                    );
                    if ext.is_bundled {
                        group_bundled.add(&row);
                        bundled_rows_tracker
                            .borrow_mut()
                            .push(row.upcast::<gtk::Widget>());
                        bundled_count += 1;
                    } else {
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
                        .subtitle(gettext("Browse Discover to add extensions"))
                        .build();
                    group_user.add(&empty_row);
                    user_rows_tracker
                        .borrow_mut()
                        .push(empty_row.upcast::<gtk::Widget>());
                }

                group_bundled.set_visible(bundled_count > 0 || q.is_empty());
                group_user.set_visible(user_count > 0 || q.is_empty());

                // Populate Discover tab with sleek, compact rows (click opens detail page)
                let mut discover_count = 0usize;
                for entry in catalog_entries.borrow().iter() {
                    let hay = format!(
                        "{} {} {} {}",
                        entry.name.to_ascii_lowercase(),
                        entry.description.to_ascii_lowercase(),
                        entry.category.to_ascii_lowercase(),
                        entry.id.to_ascii_lowercase()
                    );
                    if !q.is_empty() && !hay.contains(&q) {
                        continue;
                    }
                    let is_installed = installed_ids.contains(&entry.id);
                    let row = build_compact_discover_row(
                        entry,
                        is_installed,
                        &nav_view_ref,
                        &toast_overlay_ref,
                        Rc::clone(&rebuild_self),
                        filter_query,
                    );
                    group_discover.add(&row);
                    discover_rows_tracker
                        .borrow_mut()
                        .push(row.upcast::<gtk::Widget>());
                    discover_count += 1;
                }

                if discover_count == 0 {
                    let empty_row = adw::ActionRow::builder()
                        .title(gettext("No matching extensions found"))
                        .build();
                    group_discover.add(&empty_row);
                    discover_rows_tracker
                        .borrow_mut()
                        .push(empty_row.upcast::<gtk::Widget>());
                }
            });

            *rebuild_holder.borrow_mut() = Some(Rc::clone(&closure));
            closure
        };

        rebuild_all("");

        // Fetch optional remote community catalog off the main thread
        {
            let catalog_ref = Rc::clone(&catalog_entries);
            let rebuild_remote = Rc::clone(&rebuild_all);
            let search_weak = search_entry.downgrade();
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let entries = ExtensionCatalogService::default().fetch_catalog_blocking();
                let _ = tx.send_blocking(entries);
            });
            glib::spawn_future_local(async move {
                if let Ok(entries) = rx.recv().await {
                    let changed = *catalog_ref.borrow() != entries;
                    if changed {
                        *catalog_ref.borrow_mut() = entries;
                        let query = search_weak
                            .upgrade()
                            .map(|e| e.text().to_string())
                            .unwrap_or_default();
                        rebuild_remote(&query);
                    }
                }
            });
        }

        let rebuild_for_search = Rc::clone(&rebuild_all);
        search_entry.connect_search_changed(move |entry| {
            rebuild_for_search(&entry.text());
        });

        // Manage group rows
        let row_install = adw::ActionRow::builder()
            .title(gettext("Install Extension from Folder…"))
            .activatable(true)
            .build();
        row_install.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let dialog_weak = dialog.downgrade();
            let toast_c = toast_overlay.clone();
            let rebuild_c = Rc::clone(&rebuild_all);
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

        // Open Extensions Folder (ensures EXTENSION_GUIDE.md is present for developers & LLMs)
        let row_open_dir = adw::ActionRow::builder()
            .title(gettext("Open Extensions Folder"))
            .activatable(true)
            .build();
        row_open_dir.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
        {
            let toast_c = toast_overlay.clone();
            row_open_dir.connect_activated(move |_| {
                let dir = crate::services::extension_service::extensions_dir();
                let _ = crate::services::extension_service::ensure_extension_guide_written(&dir);
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

        // Developer & AI-assisted extension creation (collapsed by default so normal users aren't cluttered)
        let dev_expander = adw::ExpanderRow::builder()
            .title(gettext("Build Your Own Extension"))
            .subtitle(gettext("Documentation, AI guide, and starter template"))
            .use_markup(false)
            .expanded(false)
            .build();
        dev_expander.add_prefix(&gtk::Image::from_icon_name("utilities-terminal-symbolic"));

        let row_copy_guide = adw::ActionRow::builder()
            .title(gettext("Copy Guide for AI Assistants"))
            .subtitle(gettext("Paste into any LLM to generate a Meridian extension"))
            .use_markup(false)
            .activatable(true)
            .build();
        row_copy_guide.add_prefix(&gtk::Image::from_icon_name("edit-copy-symbolic"));
        {
            let toast_c = toast_overlay.clone();
            row_copy_guide.connect_activated(move |_| {
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(
                        crate::services::extension_service::EXTENSION_DEV_GUIDE_MD,
                    );
                    toast_c.add_toast(adw::Toast::new(&gettext(
                        "Extension guide copied to clipboard",
                    )));
                }
            });
        }
        dev_expander.add_row(&row_copy_guide);

        let row_open_guide = adw::ActionRow::builder()
            .title(gettext("Open Extension Guide"))
            .subtitle("EXTENSION_GUIDE.md")
            .use_markup(false)
            .activatable(true)
            .build();
        row_open_guide.add_prefix(&gtk::Image::from_icon_name("x-office-document-symbolic"));
        row_open_guide.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
        {
            let toast_c = toast_overlay.clone();
            row_open_guide.connect_activated(move |_| {
                let dir = crate::services::extension_service::extensions_dir();
                match crate::services::extension_service::ensure_extension_guide_written(&dir) {
                    Ok(guide_path) => {
                        let uri = gio::File::for_path(&guide_path).uri();
                        if let Err(err) =
                            gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
                        {
                            toast_c.add_toast(adw::Toast::new(&format!(
                                "{}: {err}",
                                gettext("Could not open file")
                            )));
                        }
                    }
                    Err(err) => {
                        toast_c.add_toast(adw::Toast::new(&err.to_string()));
                    }
                }
            });
        }
        dev_expander.add_row(&row_open_guide);

        let row_scaffold = adw::ActionRow::builder()
            .title(gettext("Create Starter Extension…"))
            .subtitle(gettext("Generate a sample folder with extension.json and script"))
            .use_markup(false)
            .activatable(true)
            .build();
        row_scaffold.add_prefix(&gtk::Image::from_icon_name("document-new-symbolic"));
        row_scaffold.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let dialog_weak = dialog.downgrade();
            let toast_c = toast_overlay.clone();
            let rebuild_c = Rc::clone(&rebuild_all);
            row_scaffold.connect_activated(move |_| {
                let Some(dlg) = dialog_weak.upgrade() else {
                    return;
                };
                present_starter_extension_prompt(&dlg, &toast_c, Rc::clone(&rebuild_c));
            });
        }
        dev_expander.add_row(&row_scaffold);
        group_tools.add(&dev_expander);

        installed_page.add(&group_bundled);
        installed_page.add(&group_user);
        installed_page.add(&group_tools);

        toast_overlay.set_child(Some(&nav_view));
        dialog.set_child(Some(&toast_overlay));

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

/// Compact row for an installed / bundled extension: icon, title, 1-line subtitle,
/// inline toggle switch, and click-to-open detail page.
fn build_compact_installed_row(
    ext: &InstalledExtension,
    nav_view: &adw::NavigationView,
    toast_overlay: &adw::ToastOverlay,
    dialog_weak: &glib::WeakRef<adw::Dialog>,
    rebuild_self: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>,
    filter_query: &str,
) -> adw::ActionRow {
    let short_subtitle = if ext.is_bundled {
        format!("{} · v{}", gettext("Built-in"), ext.manifest.version)
    } else if !ext.manifest.author.trim().is_empty() {
        format!("{} · v{}", ext.manifest.author, ext.manifest.version)
    } else {
        format!("v{}", ext.manifest.version)
    };

    let row = adw::ActionRow::builder()
        .title(&ext.manifest.name)
        .subtitle(&short_subtitle)
        .subtitle_lines(1)
        .use_markup(false)
        .activatable(true)
        .build();
    let icon = gtk::Image::builder()
        .icon_name(&ext.manifest.icon_name)
        .pixel_size(16)
        .css_classes(["dim-label"])
        .build();
    row.add_prefix(&icon);

    let toggle = gtk::Switch::builder()
        .active(ext.enabled)
        .valign(gtk::Align::Center)
        .build();
    {
        let ext_id = ext.manifest.id.clone();
        let toast_c = toast_overlay.clone();
        toggle.connect_active_notify(move |sw| {
            if let Err(err) = ExtensionRegistry::default().set_enabled(&ext_id, sw.is_active()) {
                toast_c.add_toast(adw::Toast::new(&err.to_string()));
            }
        });
    }
    row.add_suffix(&toggle);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let ext_owned = ext.clone();
    let nav_c = nav_view.clone();
    let toast_c = toast_overlay.clone();
    let dlg_w = dialog_weak.clone();
    let filter_owned = filter_query.to_string();
    let toggle_ref = toggle.clone();
    row.connect_activated(move |_| {
        let mut live_ext = ext_owned.clone();
        live_ext.enabled = toggle_ref.is_active();
        let detail = build_installed_detail_page(
            &live_ext,
            &nav_c,
            &toast_c,
            &dlg_w,
            Rc::clone(&rebuild_self),
            &filter_owned,
        );
        nav_c.push(&detail);
    });
    row
}

/// Compact row for a Discover catalog extension: icon, title, 1-line category/author,
/// subtle installed checkmark if installed, and chevron to open detail view.
fn build_compact_discover_row(
    entry: &CatalogExtensionEntry,
    is_installed: bool,
    nav_view: &adw::NavigationView,
    toast_overlay: &adw::ToastOverlay,
    rebuild_self: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>,
    filter_query: &str,
) -> adw::ActionRow {
    let subtitle = if entry.category.trim().is_empty() {
        entry.author.clone()
    } else {
        format!("{} · {}", entry.category, entry.author)
    };
    let row = adw::ActionRow::builder()
        .title(&entry.name)
        .subtitle(&subtitle)
        .subtitle_lines(1)
        .use_markup(false)
        .activatable(true)
        .build();
    let icon = gtk::Image::builder()
        .icon_name(&entry.icon_name)
        .pixel_size(16)
        .css_classes(["dim-label"])
        .build();
    row.add_prefix(&icon);

    if is_installed {
        let check = gtk::Image::builder()
            .icon_name("emblem-ok-symbolic")
            .tooltip_text(gettext("Installed"))
            .css_classes(["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&check);
    }
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let entry_owned = entry.clone();
    let nav_c = nav_view.clone();
    let toast_c = toast_overlay.clone();
    let filter_owned = filter_query.to_string();
    row.connect_activated(move |_| {
        let detail = build_discover_detail_page(
            &entry_owned,
            is_installed,
            &nav_c,
            &toast_c,
            Rc::clone(&rebuild_self),
            &filter_owned,
        );
        nav_c.push(&detail);
    });
    row
}

fn format_permission_details(perm: &str) -> (&'static str, String, String) {
    match perm {
        "network" => (
            "network-transmit-receive-symbolic",
            gettext("Network access"),
            gettext("Can connect to external servers or websites"),
        ),
        "webview" => (
            "web-browser-symbolic",
            gettext("Embedded web tab"),
            gettext("Adds a web workspace tab inside Meridian"),
        ),
        "ui_css" => (
            "applications-graphics-symbolic",
            gettext("Appearance & UI customization"),
            gettext("Can customize app styling, email CSS, or UI labels"),
        ),
        "read_mail" => (
            "mail-read-symbolic",
            gettext("Read email content & attachments"),
            gettext("Can inspect opened messages and attachments"),
        ),
        "modify_mail" => (
            "document-edit-symbolic",
            gettext("Modify email display or drafts"),
            gettext("Can transform email HTML, list badges, or drafts"),
        ),
        "shell" => (
            "utilities-terminal-symbolic",
            gettext("Run local extension scripts"),
            gettext("Executes local scripts inside the extension folder"),
        ),
        other => (
            "security-medium-symbolic",
            other.to_string(),
            String::new(),
        ),
    }
}

fn build_permission_row(perm: &str) -> adw::ActionRow {
    let (icon_name, title, subtitle) = format_permission_details(perm);
    let builder = adw::ActionRow::builder()
        .title(&title)
        .use_markup(false);
    let row = if subtitle.is_empty() {
        builder.build()
    } else {
        builder.subtitle(&subtitle).build()
    };
    let icon = gtk::Image::builder()
        .icon_name(icon_name)
        .pixel_size(16)
        .css_classes(["dim-label"])
        .build();
    row.add_prefix(&icon);
    row
}

fn build_detail_hero(
    icon_name: &str,
    name: &str,
    meta: &str,
    action_widget: Option<&gtk::Widget>,
) -> gtk::Box {
    let hero = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .margin_top(6)
        .margin_bottom(6)
        .valign(gtk::Align::Center)
        .build();
    let icon = gtk::Image::builder()
        .icon_name(icon_name)
        .pixel_size(40)
        .valign(gtk::Align::Center)
        .build();
    let text_col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let title_lbl = gtk::Label::builder()
        .label(name)
        .css_classes(["title-3"])
        .halign(gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .build();
    let meta_lbl = gtk::Label::builder()
        .label(meta)
        .css_classes(["caption", "dim-label"])
        .halign(gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .build();
    text_col.append(&title_lbl);
    text_col.append(&meta_lbl);

    hero.append(&icon);
    hero.append(&text_col);
    if let Some(widget) = action_widget {
        hero.append(widget);
    }
    hero
}

/// Detail navigation page for an extension in the Discover catalog.
fn build_discover_detail_page(
    entry: &CatalogExtensionEntry,
    is_installed: bool,
    nav_view: &adw::NavigationView,
    toast_overlay: &adw::ToastOverlay,
    rebuild_self: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>,
    filter_query: &str,
) -> adw::NavigationPage {
    let tv = adw::ToolbarView::new();
    let hb = adw::HeaderBar::new();
    tv.add_top_bar(&hb);

    let page = adw::PreferencesPage::new();

    let action_btn = if is_installed {
        gtk::Button::builder()
            .label(gettext("Installed"))
            .sensitive(false)
            .css_classes(["pill"])
            .valign(gtk::Align::Center)
            .build()
    } else {
        let btn = gtk::Button::builder()
            .label(gettext("Install"))
            .css_classes(["suggested-action", "pill"])
            .valign(gtk::Align::Center)
            .build();
        let entry_owned = entry.clone();
        let toast_c = toast_overlay.clone();
        let rebuild_c = Rc::clone(&rebuild_self);
        let filter_owned = filter_query.to_string();
        let nav_c = nav_view.clone();
        btn.connect_clicked(move |b| {
            b.set_sensitive(false);
            b.set_label(&gettext("Installing…"));
            let btn_clone = b.clone();
            let entry_bg = entry_owned.clone();
            let toast_bg = toast_c.clone();
            let rebuild_bg = Rc::clone(&rebuild_c);
            let filter_bg = filter_owned.clone();
            let nav_bg = nav_c.clone();
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let res = ExtensionCatalogService::default()
                    .install_entry(&ExtensionRegistry::default(), &entry_bg);
                let _ = tx.send_blocking(res);
            });
            glib::spawn_future_local(async move {
                match rx.recv().await {
                    Ok(Ok(manifest)) => {
                        btn_clone.set_label(&gettext("Installed"));
                        btn_clone.remove_css_class("suggested-action");
                        toast_bg.add_toast(adw::Toast::new(&format!(
                            "{}: {}",
                            gettext("Installed extension"),
                            manifest.name
                        )));
                        if let Some(f) = rebuild_bg.borrow().as_ref() {
                            f(&filter_bg);
                        }
                        nav_bg.pop();
                    }
                    Ok(Err(err)) => {
                        btn_clone.set_label(&gettext("Install"));
                        btn_clone.set_sensitive(true);
                        toast_bg.add_toast(adw::Toast::new(&err));
                    }
                    Err(_) => {
                        btn_clone.set_label(&gettext("Install"));
                        btn_clone.set_sensitive(true);
                    }
                }
            });
        });
        btn
    };

    let meta = format!("{} · {} · v{}", entry.category, entry.author, entry.version);
    let hero_group = adw::PreferencesGroup::new();
    hero_group.add(&build_detail_hero(
        &entry.icon_name,
        &entry.name,
        &meta,
        Some(action_btn.upcast_ref()),
    ));
    page.add(&hero_group);

    let about_group = adw::PreferencesGroup::builder()
        .title(gettext("About"))
        .build();
    let desc_lbl = gtk::Label::builder()
        .label(&entry.description)
        .wrap(true)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .build();
    about_group.add(&desc_lbl);
    page.add(&about_group);

    if !entry.permissions.is_empty() {
        let perm_group = adw::PreferencesGroup::builder()
            .title(gettext("Permissions"))
            .build();
        for perm in &entry.permissions {
            perm_group.add(&build_permission_row(perm));
        }
        page.add(&perm_group);
    }

    tv.set_content(Some(&page));
    adw::NavigationPage::builder()
        .title(&entry.name)
        .child(&tv)
        .build()
}

/// Detail navigation page for an installed or built-in optional extension.
fn build_installed_detail_page(
    ext: &InstalledExtension,
    nav_view: &adw::NavigationView,
    toast_overlay: &adw::ToastOverlay,
    dialog_weak: &glib::WeakRef<adw::Dialog>,
    rebuild_self: Rc<RefCell<Option<Rc<dyn Fn(&str)>>>>,
    filter_query: &str,
) -> adw::NavigationPage {
    let tv = adw::ToolbarView::new();
    let hb = adw::HeaderBar::new();
    tv.add_top_bar(&hb);

    let page = adw::PreferencesPage::new();

    let enable_switch = gtk::Switch::builder()
        .active(ext.enabled)
        .valign(gtk::Align::Center)
        .build();
    {
        let ext_id = ext.manifest.id.clone();
        let toast_c = toast_overlay.clone();
        let rebuild_c = Rc::clone(&rebuild_self);
        let filter_owned = filter_query.to_string();
        enable_switch.connect_active_notify(move |sw| {
            match ExtensionRegistry::default().set_enabled(&ext_id, sw.is_active()) {
                Ok(()) => {
                    if let Some(f) = rebuild_c.borrow().as_ref() {
                        f(&filter_owned);
                    }
                }
                Err(err) => {
                    toast_c.add_toast(adw::Toast::new(&err.to_string()));
                }
            }
        });
    }

    let meta = if ext.is_bundled {
        format!("{} · v{}", gettext("Built-in Optional Feature"), ext.manifest.version)
    } else {
        format!("{} · v{}", ext.manifest.author, ext.manifest.version)
    };
    let hero_group = adw::PreferencesGroup::new();
    hero_group.add(&build_detail_hero(
        &ext.manifest.icon_name,
        &ext.manifest.name,
        &meta,
        Some(enable_switch.upcast_ref()),
    ));
    page.add(&hero_group);

    let about_group = adw::PreferencesGroup::builder()
        .title(gettext("About"))
        .build();
    let desc_lbl = gtk::Label::builder()
        .label(&ext.manifest.description)
        .wrap(true)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .build();
    about_group.add(&desc_lbl);
    page.add(&about_group);

    // Inline extension settings + view placement right on the detail page
    if !ext.manifest.contributes.settings.is_empty() || !ext.manifest.contributes.views.is_empty() {
        let settings_group = adw::PreferencesGroup::builder()
            .title(gettext("Extension Settings"))
            .build();
        let reg = ExtensionRegistry::default();
        let stored = reg.extension_settings_values(&ext.manifest.id);

        for view in &ext.manifest.contributes.views {
            let current_placement = reg.effective_view_placement(&ext.manifest.id, view);
            let placement_model = gtk::StringList::new(&[
                &gettext("Top-Level Tab"),
                &gettext("Global Right Sidebar"),
                &gettext("Page-Specific Right Sidebar"),
            ]);
            let selected_idx = match current_placement {
                ExtensionViewPlacement::Tab => 0,
                ExtensionViewPlacement::GlobalSidebar => 1,
                ExtensionViewPlacement::PageSidebar => 2,
            };
            let title = if ext.manifest.contributes.views.len() > 1 {
                format!("{} ({})", gettext("Placement"), view.title)
            } else {
                gettext("Placement")
            };
            let combo = adw::ComboRow::builder()
                .title(&title)
                .subtitle(gettext("Where this extension appears in Meridian"))
                .use_markup(false)
                .model(&placement_model)
                .selected(selected_idx)
                .build();
            let ext_id = ext.manifest.id.clone();
            let view_id = view.id.clone();
            let toast_c = toast_overlay.clone();
            combo.connect_selected_notify(move |row| {
                let placement = match row.selected() {
                    1 => ExtensionViewPlacement::GlobalSidebar,
                    2 => ExtensionViewPlacement::PageSidebar,
                    _ => ExtensionViewPlacement::Tab,
                };
                if let Err(err) = ExtensionRegistry::default()
                    .set_view_placement_override(&ext_id, &view_id, placement)
                {
                    toast_c.add_toast(adw::Toast::new(&err.to_string()));
                }
            });
            settings_group.add(&combo);
        }

        for schema in &ext.manifest.contributes.settings {
            let current_val = stored
                .get(&schema.key)
                .cloned()
                .unwrap_or_else(|| schema.default_value.clone());
            let ext_id = ext.manifest.id.clone();
            let key = schema.key.clone();
            let toast_c = toast_overlay.clone();

            match schema.kind {
                ExtensionSettingKind::Bool => {
                    let row = adw::SwitchRow::builder()
                        .title(&schema.title)
                        .subtitle(&schema.description)
                        .use_markup(false)
                        .active(current_val.eq_ignore_ascii_case("true") || current_val == "1")
                        .build();
                    row.connect_active_notify(move |sw| {
                        let val = if sw.is_active() { "true" } else { "false" };
                        if let Err(err) =
                            ExtensionRegistry::default().set_extension_setting(&ext_id, &key, val)
                        {
                            toast_c.add_toast(adw::Toast::new(&err.to_string()));
                        }
                    });
                    settings_group.add(&row);
                }
                ExtensionSettingKind::Secret => {
                    let existing_secret = reg
                        .get_extension_secret(&ext.manifest.id, &schema.key)
                        .unwrap_or_default();
                    let row = adw::PasswordEntryRow::builder()
                        .title(&schema.title)
                        .text(&existing_secret)
                        .show_apply_button(true)
                        .build();
                    if !schema.description.trim().is_empty() {
                        row.set_tooltip_text(Some(&schema.description));
                    }
                    row.connect_apply(move |entry| {
                        let val = entry.text().trim().to_string();
                        match ExtensionRegistry::default()
                            .set_extension_secret(&ext_id, &key, &val)
                        {
                            Ok(()) => {
                                toast_c.add_toast(adw::Toast::new(&gettext(
                                    "Secret saved securely in system keyring",
                                )));
                            }
                            Err(err) => {
                                toast_c.add_toast(adw::Toast::new(&err));
                            }
                        }
                    });
                    settings_group.add(&row);
                }
                ExtensionSettingKind::String => {
                    let row = adw::EntryRow::builder()
                        .title(&schema.title)
                        .text(&current_val)
                        .show_apply_button(true)
                        .build();
                    if !schema.description.trim().is_empty() {
                        row.set_tooltip_text(Some(&schema.description));
                    }
                    row.connect_apply(move |entry| {
                        let val = entry.text().trim().to_string();
                        match ExtensionRegistry::default()
                            .set_extension_setting(&ext_id, &key, &val)
                        {
                            Ok(()) => {
                                toast_c.add_toast(adw::Toast::new(&gettext(
                                    "Extension setting saved",
                                )));
                            }
                            Err(err) => {
                                toast_c.add_toast(adw::Toast::new(&err.to_string()));
                            }
                        }
                    });
                    settings_group.add(&row);
                }
            }
        }
        page.add(&settings_group);
    }

    if !ext.manifest.permissions.is_empty() {
        let perm_group = adw::PreferencesGroup::builder()
            .title(gettext("Permissions"))
            .build();
        for perm in &ext.manifest.permissions {
            perm_group.add(&build_permission_row(perm));
        }
        page.add(&perm_group);
    }

    if !ext.is_bundled {
        let manage_group = adw::PreferencesGroup::new();
        if let Some(root_dir) = ext.root_dir.clone() {
            let open_row = adw::ActionRow::builder()
                .title(gettext("Open Extension Folder"))
                .activatable(true)
                .build();
            open_row.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
            let toast_c = toast_overlay.clone();
            open_row.connect_activated(move |_| {
                let uri = gio::File::for_path(&root_dir).uri();
                if let Err(err) =
                    gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE)
                {
                    toast_c.add_toast(adw::Toast::new(&format!(
                        "{}: {err}",
                        gettext("Could not open folder")
                    )));
                }
            });
            manage_group.add(&open_row);
        }

        let remove_row = adw::ActionRow::builder()
            .title(gettext("Remove Extension"))
            .css_classes(["error"])
            .activatable(true)
            .build();
        remove_row.add_prefix(&gtk::Image::from_icon_name("user-trash-symbolic"));
        let ext_id = ext.manifest.id.clone();
        let toast_c = toast_overlay.clone();
        let dlg_w = dialog_weak.clone();
        let nav_c = nav_view.clone();
        let filter_owned = filter_query.to_string();
        remove_row.connect_activated(move |_| {
            let Some(dlg) = dlg_w.upgrade() else {
                return;
            };
            let id_owned = ext_id.clone();
            let toast_rm = toast_c.clone();
            let rebuild_rm = Rc::clone(&rebuild_self);
            let nav_rm = nav_c.clone();
            let filter_rm = filter_owned.clone();
            crate::dialogs::run_destructive_action(
                &dlg,
                &gettext("Remove Extension?"),
                &gettext("The extension folder will be removed from local storage."),
                &gettext("Remove"),
                move || match ExtensionRegistry::default().uninstall(&id_owned) {
                    Ok(()) => {
                        toast_rm.add_toast(adw::Toast::new(&gettext("Extension removed")));
                        if let Some(f) = rebuild_rm.borrow().as_ref() {
                            f(&filter_rm);
                        }
                        nav_rm.pop();
                    }
                    Err(err) => {
                        toast_rm.add_toast(adw::Toast::new(&err));
                    }
                },
            );
        });
        manage_group.add(&remove_row);
        page.add(&manage_group);
    }

    tv.set_content(Some(&page));
    adw::NavigationPage::builder()
        .title(&ext.manifest.name)
        .child(&tv)
        .build()
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
