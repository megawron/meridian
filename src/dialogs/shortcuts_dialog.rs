// SPDX-License-Identifier: GPL-3.0-or-later
//
// ShortcutsDialog — Keyboard shortcuts window listing GNOME Evolution GTK4 accelerators.

use gtk::prelude::*;

pub fn show_shortcuts_dialog<W: IsA<gtk::Window>>(parent: &W) {
    let window = gtk::ShortcutsWindow::builder()
        .transient_for(parent)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .max_height(10)
        .build();

    // Group 1: General & Navigation
    let group_general = gtk::ShortcutsGroup::builder()
        .title("Allgemein & Navigation")
        .build();

    let s1 = gtk::ShortcutsShortcut::builder()
        .title("Einstellungen öffnen")
        .accelerator("<Control>comma")
        .build();
    let s2 = gtk::ShortcutsShortcut::builder()
        .title("Tastaturkürzel anzeigen")
        .accelerator("<Control>question")
        .build();
    let s3 = gtk::ShortcutsShortcut::builder()
        .title("Anwendungsmenü / Beenden")
        .accelerator("<Control>q")
        .build();

    group_general.append(&s1);
    group_general.append(&s2);
    group_general.append(&s3);
    section.append(&group_general);

    // Group 2: Mail Actions
    let group_mail = gtk::ShortcutsGroup::builder()
        .title("E-Mail Aktionen")
        .build();

    let m1 = gtk::ShortcutsShortcut::builder()
        .title("Neue E-Mail verfassen")
        .accelerator("<Control>n")
        .build();
    let m2 = gtk::ShortcutsShortcut::builder()
        .title("Antworten")
        .accelerator("<Control>r")
        .build();
    let m3 = gtk::ShortcutsShortcut::builder()
        .title("Allen antworten")
        .accelerator("<Control><Shift>r")
        .build();
    let m4 = gtk::ShortcutsShortcut::builder()
        .title("Weiterleiten")
        .accelerator("<Control>f")
        .build();
    let m5 = gtk::ShortcutsShortcut::builder()
        .title("Nachricht löschen")
        .accelerator("Delete")
        .build();

    group_mail.append(&m1);
    group_mail.append(&m2);
    group_mail.append(&m3);
    group_mail.append(&m4);
    group_mail.append(&m5);
    section.append(&group_mail);

    // Group 3: Calendar & Contacts
    let group_groupware = gtk::ShortcutsGroup::builder()
        .title("Kalender & Kontakte")
        .build();

    let g1 = gtk::ShortcutsShortcut::builder()
        .title("Neuer Termin")
        .accelerator("<Control><Shift>e")
        .build();
    let g2 = gtk::ShortcutsShortcut::builder()
        .title("Neuer Kontakt")
        .accelerator("<Control><Shift>c")
        .build();

    group_groupware.append(&g1);
    group_groupware.append(&g2);
    section.append(&group_groupware);

    window.set_child(Some(&section));
    window.present();
}
