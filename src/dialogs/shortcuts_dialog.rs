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

/// Helper that adds a `gtk::ShortcutsSection` to `window` and arranges its groups
/// into balanced side-by-side columns on a single page without triggering GTK 4.22's
/// multi-page reflow bugs or negative min-width label warnings.
#[allow(deprecated)]
fn add_section_with_columns(
    window: &gtk::ShortcutsWindow,
    section_name: &str,
    title: &str,
    columns: Vec<Vec<gtk::ShortcutsGroup>>,
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
            .title(&crate::i18n::gettext(title))
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
            .title(&crate::i18n::gettext(spec.title))
            .accelerator(&accel)
            .build();
        group.add_shortcut(&shortcut);
    }
    group
}

/// Creates a fully populated `gtk::ShortcutsWindow` matching Evolution / GNOME HIG standards.
#[allow(deprecated)]
pub fn create_shortcuts_window<W: IsA<gtk::Window>>(
    parent: Option<&W>,
    initial_section: Option<&str>,
) -> gtk::ShortcutsWindow {
    let builder = gtk::ShortcutsWindow::builder().modal(true);
    let window = if let Some(p) = parent {
        builder.transient_for(p).build()
    } else {
        builder.build()
    };

    // 1. Mail section (3 balanced columns)
    let mail_nav = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailNavigation,
        &crate::i18n::gettext("Navigation & Search"),
        &[],
    );
    let mail_view = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailView,
        &crate::i18n::gettext("View & Zoom"),
        &[],
    );
    let mail_msg = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailMessage,
        &crate::i18n::gettext("Message Actions"),
        &[],
    );
    let mail_org = build_spec_group(
        ShortcutSection::Mail,
        ShortcutGroup::MailOrganize,
        &crate::i18n::gettext("Organize & Flags"),
        &[],
    );
    add_section_with_columns(
        &window,
        "mail",
        &crate::i18n::gettext("Mail"),
        vec![vec![mail_nav, mail_view], vec![mail_msg], vec![mail_org]],
    );

    // 2. Composer section (2 columns)
    let comp_send_group = gtk::ShortcutsGroup::builder()
        .title(&crate::i18n::gettext("Send & Save"))
        .build();
    for (title, accel) in [
        (crate::i18n::gettext("Send Message"), "<Control>Return"),
        (crate::i18n::gettext("Save Draft"), "<Control>s"),
        (crate::i18n::gettext("Save as EML"), "<Control><Shift>s"),
        (crate::i18n::gettext("Close / Discard"), "<Control>w Escape"),
    ] {
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(&title)
            .accelerator(accel)
            .build();
        comp_send_group.add_shortcut(&shortcut);
    }

    let comp_format_group = gtk::ShortcutsGroup::builder()
        .title(&crate::i18n::gettext("Formatting & Options"))
        .build();
    for (title, accel) in [
        (crate::i18n::gettext("Attach File"), "<Control>m"),
        (crate::i18n::gettext("Toggle Formatting Toolbar"), "<Control><Shift>f"),
        (crate::i18n::gettext("Print Draft"), "<Control>p"),
        (crate::i18n::gettext("Toggle Fullscreen"), "F11"),
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
        &crate::i18n::gettext("Composer"),
        vec![vec![comp_send_group], vec![comp_format_group]],
    );

    // 3. Calendar section (2 columns)
    let cal_events_group = build_spec_group(
        ShortcutSection::Calendar,
        ShortcutGroup::CalendarEvents,
        &crate::i18n::gettext("Events & Appointments"),
        &[
            ("New Event", "<Control>n"),
            ("Focus Search", "<Control>f"),
            ("Delete Selected Event", "Delete"),
        ],
    );
    let cal_nav_group = gtk::ShortcutsGroup::builder()
        .title(&crate::i18n::gettext("Navigation & Views"))
        .build();
    for (title, accel) in [
        (crate::i18n::gettext("Go to Today"), "<Control>t"),
        (crate::i18n::gettext("Day View"), "<Control>1"),
        (crate::i18n::gettext("Work Week View"), "<Control>2"),
        (crate::i18n::gettext("Week View"), "<Control>3"),
        (crate::i18n::gettext("Month View"), "<Control>4"),
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
        &crate::i18n::gettext("Calendar"),
        vec![vec![cal_events_group], vec![cal_nav_group]],
    );

    // 4. Contacts & Tasks section (2 columns)
    let contacts_group = build_spec_group(
        ShortcutSection::ContactsTasks,
        ShortcutGroup::Contacts,
        &crate::i18n::gettext("Contacts"),
        &[
            ("New Contact", "<Control>n"),
            ("Focus Search", "<Control>f"),
            ("Delete Contact", "Delete"),
        ],
    );
    let tasks_group = build_spec_group(
        ShortcutSection::ContactsTasks,
        ShortcutGroup::TasksMemos,
        &crate::i18n::gettext("Tasks & Memos"),
        &[
            ("New Task / Memo", "<Control>n"),
            ("Focus Search", "<Control>f"),
            ("Delete Selected Item", "Delete"),
        ],
    );
    add_section_with_columns(
        &window,
        "contacts_tasks",
        &crate::i18n::gettext("Contacts & Tasks"),
        vec![vec![contacts_group], vec![tasks_group]],
    );

    // 5. General section (2 columns)
    let gen_modules = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralModules,
        &crate::i18n::gettext("Switch Modules"),
        &[],
    );
    let gen_panes = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralPanes,
        &crate::i18n::gettext("Panes & Views"),
        &[],
    );
    let gen_app = build_spec_group(
        ShortcutSection::General,
        ShortcutGroup::GeneralApp,
        &crate::i18n::gettext("Application & Search"),
        &[],
    );
    add_section_with_columns(
        &window,
        "general",
        &crate::i18n::gettext("General"),
        vec![vec![gen_modules, gen_panes], vec![gen_app]],
    );

    // Context-aware initial section selection
    let target_section = match initial_section {
        Some("composer") => "composer",
        Some("calendar") => "calendar",
        Some("contacts") | Some("tasks") | Some("memos") => "contacts_tasks",
        Some("mail") => "mail",
        Some("general") => "general",
        Some(other) => other,
        None => "mail",
    };
    window.set_section_name(Some(target_section));

    window
}

pub fn show_shortcuts_dialog<W: IsA<gtk::Window>>(parent: &W) {
    show_shortcuts_dialog_for_view(Some(parent), None);
}

pub fn show_shortcuts_dialog_for_view<W: IsA<gtk::Window>>(
    parent: Option<&W>,
    active_view: Option<&str>,
) {
    let window = create_shortcuts_window(parent, active_view);
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_shortcuts_window_construction_and_sections() {
        let Some(_context) = crate::window::gtk_test_main_context() else {
            return;
        };

        let window = create_shortcuts_window::<gtk::Window>(None, Some("calendar"));
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
}
