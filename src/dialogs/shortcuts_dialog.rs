use crate::i18n::gettext;
// SPDX-License-Identifier: GPL-3.0-or-later
//
// ShortcutsDialog — Keyboard shortcuts window listing GNOME Evolution GTK4 accelerators.

use gtk::prelude::*;

use crate::action_contract::{SHORTCUTS, ShortcutGroup, ShortcutSection};

/// Formats registered accelerators for a given action for clean GtkShortcutsShortcut display.
/// Secondary keys (e.g. Vim navigation `J` and `K`) and primary shortcuts are retained,
/// while duplicate numpad aliases are omitted for visual clarity.
pub(crate) fn format_accelerators_for_display(action: &str) -> String {
    if let Some(accels) = crate::application::all_accelerators(action) {
        let filtered: Vec<&str> = accels
            .iter()
            .copied()
            .filter(|a| {
                !a.ends_with("KP_Add")
                    && !a.ends_with("KP_Subtract")
                    && !a.ends_with("KP_0")
                    && !a.ends_with("slash")
            })
            .collect();
        if !filtered.is_empty() {
            return filtered.join(" ");
        }
        return accels.join(" ");
    }
    crate::application::primary_accelerator(action)
        .unwrap_or("")
        .to_string()
}

/// Metadata for a group of shortcuts inside `gtk::ShortcutsWindow`, used to insert
/// contextual section/group subheadings into the flat search results list.
struct SearchGroupMeta {
    section_title: String,
    group_title: String,
    shortcut_count: usize,
}

#[allow(deprecated)]
fn count_group_shortcuts(group: &gtk::ShortcutsGroup) -> usize {
    let mut count = 0;
    let mut child = group.first_child();
    while let Some(w) = child {
        if w.is::<gtk::ShortcutsShortcut>() {
            count += 1;
        }
        child = w.next_sibling();
    }
    count
}

fn find_search_results_shortcuts_box(root: &gtk::Widget) -> Option<gtk::Box> {
    if let Some(b) = root.downcast_ref::<gtk::Box>() {
        if b.has_css_class("shortcuts-search-results") {
            return b.first_child().and_downcast::<gtk::Box>();
        }
    }
    let mut child = root.first_child();
    while let Some(w) = child {
        if let Some(found) = find_search_results_shortcuts_box(&w) {
            return Some(found);
        }
        child = w.next_sibling();
    }
    None
}

/// Inserts contextual section/group subheadings into
/// `GtkShortcutsWindow`'s internal search results list so duplicate accelerators
/// across different views (e.g. `Ctrl+N`, `Ctrl+F`, `Delete`, `Ctrl+M`, `Ctrl+P`)
/// clearly indicate which view and group they belong to.
#[allow(deprecated)]
fn install_search_result_subheadings(
    window: &gtk::ShortcutsWindow,
    groups: &[SearchGroupMeta],
) {
    let Some(search_shortcuts_box) =
        find_search_results_shortcuts_box(window.upcast_ref::<gtk::Widget>())
    else {
        return;
    };

    let mut shortcut_items = Vec::new();
    let mut child = search_shortcuts_box.first_child();
    while let Some(w) = child {
        if w.is::<gtk::ShortcutsShortcut>() {
            shortcut_items.push(w.clone());
        }
        child = w.next_sibling();
    }

    let mut offset = 0usize;
    for meta in groups {
        if meta.shortcut_count == 0 || offset + meta.shortcut_count > shortcut_items.len() {
            offset = (offset + meta.shortcut_count).min(shortcut_items.len());
            continue;
        }
        let group_slice = &shortcut_items[offset..offset + meta.shortcut_count];
        offset += meta.shortcut_count;

        let heading_text = format!("{} — {}", meta.section_title, meta.group_title);
        let header_label = gtk::Label::builder()
            .label(&heading_text)
            .xalign(0.0)
            .halign(gtk::Align::Start)
            .margin_top(12)
            .margin_bottom(4)
            .css_classes(["heading", "shortcuts-search-subheading"])
            .build();
        header_label.set_widget_name("shortcuts-search-subheading");
        header_label.insert_before(&search_shortcuts_box, Some(&group_slice[0]));

        let group_widgets: Vec<gtk::Widget> = group_slice.to_vec();
        let any_initial = group_widgets.iter().any(|w| w.property::<bool>("visible"));
        header_label.set_visible(any_initial);

        for item in &group_widgets {
            let header_weak = header_label.downgrade();
            let widgets_for_notify = group_widgets.clone();
            item.connect_visible_notify(move |_| {
                if let Some(header) = header_weak.upgrade() {
                    let any_visible = widgets_for_notify
                        .iter()
                        .any(|w| w.property::<bool>("visible"));
                    if header.property::<bool>("visible") != any_visible {
                        header.set_visible(any_visible);
                    }
                }
            });
        }
    }
}

/// Helper that adds a `gtk::ShortcutsSection` to `window` and arranges its groups
/// into balanced side-by-side columns on a single page without triggering GTK 4.22's
/// multi-page reflow bugs or negative min-width label warnings.
#[allow(deprecated)]
fn add_section_with_columns(
    window: &gtk::ShortcutsWindow,
    section_name: &str,
    title: &str,
    columns: Vec<Vec<gtk::ShortcutsGroup>>,
    search_groups: &mut Vec<SearchGroupMeta>,
) {
    let max_h = columns
        .iter()
        .map(|col| {
            col.iter()
                .map(|g| g.property::<u32>("height"))
                .sum::<u32>()
        })
        .max()
        .unwrap_or(15)
        .max(5);
    let section = gtk::ShortcutsSection::builder()
        .section_name(section_name)
        .title(title)
        .max_height(max_h)
        .build();
    for col_groups in &columns {
        for group in col_groups {
            let group_title = group.property::<String>("title");
            let shortcut_count = count_group_shortcuts(group);
            search_groups.push(SearchGroupMeta {
                section_title: title.to_string(),
                group_title,
                shortcut_count,
            });
            section.add_group(group);
        }
    }
    window.add_section(&section);
}

#[allow(deprecated)]
fn build_spec_group(
    section: ShortcutSection,
    group_kind: ShortcutGroup,
    group_title: &str,
    extra_shortcuts: &[(&str, &str)],
) -> gtk::ShortcutsGroup {
    let group = gtk::ShortcutsGroup::builder().title(group_title).build();
    for &(title, accel) in extra_shortcuts {
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(gettext(title))
            .accelerator(accel)
            .build();
        group.add_shortcut(&shortcut);
    }
    for spec in SHORTCUTS
        .iter()
        .filter(|s| s.section == section && s.group == group_kind)
    {
        let accel = format_accelerators_for_display(spec.action);
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(gettext(spec.title))
            .accelerator(&accel)
            .build();
        group.add_shortcut(&shortcut);
    }
    group
}

/// Creates a fully populated non-modal `gtk::ShortcutsWindow` matching Evolution / GNOME HIG standards.
#[allow(deprecated)]
pub fn create_shortcuts_window<W: IsA<gtk::Window>>(
    parent: Option<&W>,
    initial_section: Option<&str>,
) -> gtk::ShortcutsWindow {
    let window = gtk::ShortcutsWindow::builder().modal(false).build();
    if let Some(p) = parent {
        if let Some(app) = p.application() {
            window.set_application(Some(&app));
        }
    }

    let mut search_groups = Vec::new();

    // 1. Mail section (3 balanced columns)
    let mail_nav = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailNavigation,
        &gettext("Navigation & Search"),
        &[
            ("Scroll Message / Next Unread", "space"),
            ("Scroll Message Up", "<Shift>space"),
            ("Expand / Collapse Thread", "Right Left"),
            ("Open Message", "Return"),
        ],
    );
    let mail_view = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailView,
        &gettext("View & Zoom"),
        &[],
    );
    let mail_msg = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailMessage,
        &gettext("Message Actions"),
        &[],
    );
    let mail_org = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailOrganize,
        &gettext("Organize & Flags"),
        &[],
    );
    add_section_with_columns(
        &window,
        "mail",
        &gettext("Mail"),
        vec![vec![mail_nav], vec![mail_msg, mail_view], vec![mail_org]],
        &mut search_groups,
    );

    // 2. Composer section (2 columns)
    let comp_send_group = gtk::ShortcutsGroup::builder()
        .title(gettext("Send & Save"))
        .build();
    for (title, accel) in [
        (gettext("Send Message"), "<Control>Return"),
        (gettext("Save Draft"), "<Control>s"),
        (gettext("Save as EML"), "<Control><Shift>s"),
        (gettext("New Message"), "<Control>n"),
        (gettext("Close / Discard"), "<Control>w Escape"),
    ] {
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(&title)
            .accelerator(accel)
            .build();
        comp_send_group.add_shortcut(&shortcut);
    }

    let comp_format_group = gtk::ShortcutsGroup::builder()
        .title(gettext("Formatting & Options"))
        .build();
    for (title, accel) in [
        (gettext("Attach File"), "<Control>m"),
        (gettext("Toggle Formatting Toolbar"), "<Control><Shift>f"),
        (gettext("Print Draft"), "<Control>p"),
        (gettext("Toggle Maximize"), "<Alt>F10"),
        (gettext("Toggle Fullscreen"), "F11"),
    ] {
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(&title)
            .accelerator(accel)
            .build();
        comp_format_group.add_shortcut(&shortcut);
    }
    add_section_with_columns(
        &window,
        "composer",
        &gettext("Composer"),
        vec![vec![comp_send_group], vec![comp_format_group]],
        &mut search_groups,
    );

    // 3. Calendar section (2 columns)
    let cal_events_group = build_spec_group(
        ShortcutSection::Calendar,
        ShortcutGroup::CalendarEvents,
        &gettext("Events & Appointments"),
        &[
            ("New Event", "<Control>n"),
            ("Open / Quick Add Event", "Return"),
            ("Focus Search", "<Control>f"),
            ("Delete Selected Event", "Delete"),
            ("Print Calendar", "<Control>p"),
            ("Toggle Task & Memo Pane", "<Control>m"),
            ("Toggle Agenda Details", "<Control><Shift>d"),
            ("Refresh Calendar", "F5"),
        ],
    );
    let cal_nav_group = gtk::ShortcutsGroup::builder()
        .title(gettext("Navigation & Views"))
        .build();
    for (title, accel) in [
        (gettext("Go to Today"), "<Control>t T"),
        (gettext("Previous / Next Period"), "<Alt>Left <Alt>Right"),
        (gettext("Navigate Grid / Days"), "Up Down Left Right"),
        (gettext("Day View"), "<Alt>1 D"),
        (gettext("Work Week View"), "<Alt>2 2"),
        (gettext("Week View"), "<Alt>3 W"),
        (gettext("Month View"), "<Alt>4 M"),
        (gettext("Agenda View"), "<Alt>5 A"),
        (gettext("Year View"), "<Alt>6 Y"),
    ] {
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(&title)
            .accelerator(accel)
            .build();
        cal_nav_group.add_shortcut(&shortcut);
    }
    add_section_with_columns(
        &window,
        "calendar",
        &gettext("Calendar"),
        vec![vec![cal_events_group], vec![cal_nav_group]],
        &mut search_groups,
    );

    // 4. Contacts & Tasks section (2 columns)
    let contacts_group = build_spec_group(
        ShortcutSection::ContactsTasks,
        ShortcutGroup::Contacts,
        &gettext("Contacts"),
        &[
            ("New Contact", "<Control>n"),
            ("Edit Selected Contact", "Return"),
            ("Focus Search", "<Control>f"),
            ("Select All Contacts", "<Control>a"),
            ("Print Contacts", "<Control>p"),
            ("Delete Contact", "Delete"),
        ],
    );
    let tasks_group = build_spec_group(
        ShortcutSection::ContactsTasks,
        ShortcutGroup::TasksMemos,
        &gettext("Tasks & Memos"),
        &[
            ("New Task / Memo", "<Control>n"),
            ("Edit Selected Task", "Return"),
            ("Toggle Task Completed", "space"),
            ("Focus Search", "<Control>f"),
            ("Toggle Details Pane", "<Control>m"),
            ("Purge Completed Tasks", "<Control>e"),
            ("Print Tasks", "<Control>p"),
            ("Delete Selected Item", "Delete"),
        ],
    );
    add_section_with_columns(
        &window,
        "contacts_tasks",
        &gettext("Contacts & Tasks"),
        vec![vec![contacts_group], vec![tasks_group]],
        &mut search_groups,
    );

    // 5. General section (2 columns)
    let gen_modules = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralModules,
        &gettext("Switch Modules"),
        &[],
    );
    let gen_panes = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralPanes,
        &gettext("Panes & Views"),
        &[],
    );
    let gen_app = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralApp,
        &gettext("Application & Search"),
        &[],
    );
    add_section_with_columns(
        &window,
        "general",
        &gettext("General"),
        vec![vec![gen_modules, gen_panes], vec![gen_app]],
        &mut search_groups,
    );

    // Insert contextual view/group subheadings into the flat search results list
    install_search_result_subheadings(&window, &search_groups);

    // Context-aware initial section selection
    let target_section = resolve_target_section(initial_section);
    window.set_section_name(Some(target_section));

    window
}

fn resolve_target_section(initial_section: Option<&str>) -> &str {
    match initial_section {
        Some("composer") => "composer",
        Some("calendar") => "calendar",
        Some("contacts") | Some("tasks") | Some("memos") => "contacts_tasks",
        Some("mail") => "mail",
        Some("general") => "general",
        Some(other) => other,
        None => "mail",
    }
}

thread_local! {
    #[allow(deprecated)]
    static ACTIVE_SHORTCUTS_WINDOW: std::cell::RefCell<glib::WeakRef<gtk::ShortcutsWindow>> =
        std::cell::RefCell::new(glib::WeakRef::new());
}

pub fn show_shortcuts_dialog<W: IsA<gtk::Window>>(parent: &W) {
    show_shortcuts_dialog_for_view(Some(parent), None);
}

#[allow(deprecated)]
pub fn show_shortcuts_dialog_for_view<W: IsA<gtk::Window>>(
    parent: Option<&W>,
    active_view: Option<&str>,
) {
    let existing = ACTIVE_SHORTCUTS_WINDOW.with(|cell| cell.borrow().upgrade());
    if let Some(window) = existing {
        if active_view.is_some() {
            window.set_section_name(Some(resolve_target_section(active_view)));
        }
        window.present();
        return;
    }

    let window = create_shortcuts_window(parent, active_view);
    ACTIVE_SHORTCUTS_WINDOW.with(|cell| {
        cell.borrow_mut().set(Some(&window));
    });
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_shortcuts_window_construction_and_sections() {
        let _gtk_lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let window = create_shortcuts_window::<gtk::Window>(None, Some("calendar"));
        assert!(!window.is_modal());
        assert_eq!(window.section_name().as_deref(), Some("calendar"));

        let mail_window = create_shortcuts_window::<gtk::Window>(None, Some("mail"));
        assert_eq!(mail_window.section_name().as_deref(), Some("mail"));

        let tasks_window = create_shortcuts_window::<gtk::Window>(None, Some("tasks"));
        assert_eq!(tasks_window.section_name().as_deref(), Some("contacts_tasks"));

        let comp_window = create_shortcuts_window::<gtk::Window>(None, Some("composer"));
        assert_eq!(comp_window.section_name().as_deref(), Some("composer"));

        mail_window.present();
        for _ in 0..20 {
            while _context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            mail_window.height() > 200 && mail_window.height() < 950,
            "ShortcutsWindow height ({}) must fit on a 1080p display without vertical overflow",
            mail_window.height()
        );
        mail_window.close();
    }

    #[test]
    fn test_format_accelerators_for_display() {
        let next_accel = format_accelerators_for_display("win.mail-next");
        assert!(next_accel.contains("Page_Down"));
        assert!(next_accel.contains("J"));

        let prev_accel = format_accelerators_for_display("win.mail-prev");
        assert!(prev_accel.contains("Page_Up"));
        assert!(prev_accel.contains("K"));

        let help_accel = format_accelerators_for_display("win.show-help-overlay");
        assert!(help_accel.contains("question"));
        assert!(help_accel.contains("F1"));
    }

    #[test]
    #[allow(deprecated)]
    fn test_shortcuts_search_subheadings_inserted_and_synced() {
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let window = create_shortcuts_window::<gtk::Window>(None, Some("mail"));
        let search_box = find_search_results_shortcuts_box(window.upcast_ref::<gtk::Widget>())
            .expect("ShortcutsWindow should contain shortcuts-search-results inner box");

        let mut subheadings = Vec::new();
        let mut shortcuts = Vec::new();
        let mut child = search_box.first_child();
        while let Some(w) = child {
            if let Some(lbl) = w.downcast_ref::<gtk::Label>() {
                if lbl.has_css_class("shortcuts-search-subheading") {
                    subheadings.push(lbl.clone());
                }
            } else if w.is::<gtk::ShortcutsShortcut>() {
                shortcuts.push(w.clone());
            }
            child = w.next_sibling();
        }

        assert!(
            subheadings.len() >= 10,
            "Expected section/group subheadings for all shortcut groups, found {}",
            subheadings.len()
        );
        let texts: Vec<String> = subheadings.iter().map(|l| l.label().to_string()).collect();
        assert!(texts.iter().any(|t| t.contains("Mail") && t.contains("Navigation")));
        assert!(texts.iter().any(|t| t.contains("Calendar") && t.contains("Navigation")));
        assert!(texts.iter().any(|t| t.contains("Contacts")));
        assert!(texts.iter().any(|t| t.contains("Composer")));

        // Simulate search filtering where only the very first shortcut is visible:
        for (idx, s) in shortcuts.iter().enumerate() {
            s.set_visible(idx == 0);
        }
        assert!(
            subheadings[0].property::<bool>("visible"),
            "First group subheading should remain visible when its first shortcut matches"
        );
        assert!(
            subheadings
                .iter()
                .skip(1)
                .all(|h| !h.property::<bool>("visible")),
            "Other group subheadings should hide when none of their shortcuts match"
        );
    }
}
