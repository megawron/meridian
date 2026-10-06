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

/// Creates a fully populated `gtk::ShortcutsWindow` matching Evolution / GNOME HIG standards.
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

    // 1. Mail section
    let mail_section = gtk::ShortcutsSection::builder()
        .section_name("mail")
        .title(crate::i18n::gettext("Mail"))
        .max_height(14)
        .build();
    for (group_kind, group_title) in [
        (ShortcutGroup::MailNavigation, crate::i18n::gettext("Navigation & Search")),
        (ShortcutGroup::MailMessage, crate::i18n::gettext("Message Actions")),
        (ShortcutGroup::MailOrganize, crate::i18n::gettext("Organize & Flags")),
        (ShortcutGroup::MailView, crate::i18n::gettext("View & Zoom")),
    ] {
        let group = gtk::ShortcutsGroup::builder().title(&group_title).build();
        for spec in SHORTCUTS
            .iter()
            .filter(|s| s.section == ShortcutSection::Mail && s.group == group_kind)
        {
            let accel = format_accelerators_for_display(spec.action);
            let shortcut = gtk::ShortcutsShortcut::builder()
                .title(&crate::i18n::gettext(spec.title))
                .accelerator(&accel)
                .build();
            group.append(&shortcut);
        }
        mail_section.append(&group);
    }
    window.add_section(&mail_section);

    // 2. Composer section
    let composer_section = gtk::ShortcutsSection::builder()
        .section_name("composer")
        .title(crate::i18n::gettext("Composer"))
        .max_height(10)
        .build();
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
        comp_send_group.append(&shortcut);
    }
    composer_section.append(&comp_send_group);

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
        comp_format_group.append(&shortcut);
    }
    composer_section.append(&comp_format_group);
    window.add_section(&composer_section);

    // 3. Calendar section
    let cal_section = gtk::ShortcutsSection::builder()
        .section_name("calendar")
        .title(crate::i18n::gettext("Calendar"))
        .max_height(10)
        .build();
    let cal_group = gtk::ShortcutsGroup::builder()
        .title(&crate::i18n::gettext("Events & Appointments"))
        .build();
    for spec in SHORTCUTS
        .iter()
        .filter(|s| s.section == ShortcutSection::Calendar)
    {
        let accel = format_accelerators_for_display(spec.action);
        let shortcut = gtk::ShortcutsShortcut::builder()
            .title(&crate::i18n::gettext(spec.title))
            .accelerator(&accel)
            .build();
        cal_group.append(&shortcut);
    }
    cal_section.append(&cal_group);
    window.add_section(&cal_section);

    // 4. Contacts & Tasks section
    let ct_section = gtk::ShortcutsSection::builder()
        .section_name("contacts_tasks")
        .title(crate::i18n::gettext("Contacts & Tasks"))
        .max_height(10)
        .build();
    for (group_kind, group_title) in [
        (ShortcutGroup::Contacts, crate::i18n::gettext("Contacts")),
        (ShortcutGroup::TasksMemos, crate::i18n::gettext("Tasks & Memos")),
    ] {
        let group = gtk::ShortcutsGroup::builder().title(&group_title).build();
        for spec in SHORTCUTS.iter().filter(|s| {
            s.section == ShortcutSection::ContactsTasks && s.group == group_kind
        }) {
            let accel = format_accelerators_for_display(spec.action);
            let shortcut = gtk::ShortcutsShortcut::builder()
                .title(&crate::i18n::gettext(spec.title))
                .accelerator(&accel)
                .build();
            group.append(&shortcut);
        }
        ct_section.append(&group);
    }
    window.add_section(&ct_section);

    // 5. General section
    let general_section = gtk::ShortcutsSection::builder()
        .section_name("general")
        .title(crate::i18n::gettext("General"))
        .max_height(12)
        .build();
    for (group_kind, group_title) in [
        (ShortcutGroup::GeneralModules, crate::i18n::gettext("Switch Modules")),
        (ShortcutGroup::GeneralApp, crate::i18n::gettext("Application & Search")),
        (ShortcutGroup::GeneralPanes, crate::i18n::gettext("Panes & Views")),
    ] {
        let group = gtk::ShortcutsGroup::builder().title(&group_title).build();
        for spec in SHORTCUTS
            .iter()
            .filter(|s| s.section == ShortcutSection::General && s.group == group_kind)
        {
            let accel = format_accelerators_for_display(spec.action);
            let shortcut = gtk::ShortcutsShortcut::builder()
                .title(&crate::i18n::gettext(spec.title))
                .accelerator(&accel)
                .build();
            group.append(&shortcut);
        }
        general_section.append(&group);
    }
    window.add_section(&general_section);

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
