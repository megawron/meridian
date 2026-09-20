// SPDX-License-Identifier: GPL-3.0-or-later
//
// ShortcutsDialog — Keyboard shortcuts window listing GNOME Evolution GTK4 accelerators.

use gtk::prelude::*;

use crate::action_contract::{SHORTCUTS, ShortcutGroup};
use crate::application::primary_accelerator;

pub fn show_shortcuts_dialog<W: IsA<gtk::Window>>(parent: &W) {
    let window = gtk::ShortcutsWindow::builder()
        .transient_for(parent)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .max_height(10)
        .build();

    for (kind, title) in [
        (ShortcutGroup::General, crate::i18n::gettext("General & Navigation")),
        (ShortcutGroup::Mail, crate::i18n::gettext("Mail Actions")),
        (ShortcutGroup::Modules, crate::i18n::gettext("Switch Modules")),
    ] {
        let group = gtk::ShortcutsGroup::builder().title(&title).build();
        for spec in SHORTCUTS.iter().filter(|spec| spec.group == kind) {
            let accelerator = primary_accelerator(spec.action)
                .expect("shortcut contract must reference an accelerator binding");
            let shortcut = gtk::ShortcutsShortcut::builder()
                .title(&crate::i18n::gettext(spec.title))
                .accelerator(accelerator)
                .build();
            group.append(&shortcut);
        }
        section.append(&group);
    }

    window.set_child(Some(&section));
    window.present();
}
