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
    let s4 = gtk::ShortcutsShortcut::builder()
        .title("Mail aktualisieren")
        .accelerator("F5")
        .build();
    let s5 = gtk::ShortcutsShortcut::builder()
        .title("Sidebar ein-/ausblenden")
        .accelerator("F9")
        .build();
    let s6 = gtk::ShortcutsShortcut::builder()
        .title("Suche im aktiven Modul fokussieren")
        .accelerator("<Control>f")
        .build();
    let s7 = gtk::ShortcutsShortcut::builder()
        .title("Globale lokale Suche")
        .accelerator("<Control><Shift>f")
        .build();

    let s8 = gtk::ShortcutsShortcut::builder()
        .title("Rückgängig")
        .accelerator("<Control>z")
        .build();
    let s9 = gtk::ShortcutsShortcut::builder()
        .title("Wiederholen")
        .accelerator("<Control><Shift>z")
        .build();

    group_general.append(&s1);
    group_general.append(&s2);
    group_general.append(&s3);
    group_general.append(&s4);
    group_general.append(&s5);
    group_general.append(&s6);
    group_general.append(&s7);
    group_general.append(&s8);
    group_general.append(&s9);
    section.append(&group_general);

    // Group 2: Mail Actions
    let group_mail = gtk::ShortcutsGroup::builder()
        .title("E-Mail Aktionen")
        .build();

    let m1 = gtk::ShortcutsShortcut::builder()
        .title("Neue Nachricht verfassen")
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
        .accelerator("<Control>l")
        .build();
    let m5 = gtk::ShortcutsShortcut::builder()
        .title("Nachricht löschen")
        .accelerator("Delete")
        .build();
    let m6 = gtk::ShortcutsShortcut::builder()
        .title("Alle Nachrichten auswählen")
        .accelerator("<Control>a")
        .build();
    let m7 = gtk::ShortcutsShortcut::builder()
        .title("Wichtigkeit umschalten")
        .accelerator("<Control>i")
        .build();
    let m8 = gtk::ShortcutsShortcut::builder()
        .title("Gelesen markieren")
        .accelerator("<Control>k")
        .build();
    let m9 = gtk::ShortcutsShortcut::builder()
        .title("Ungelesen markieren")
        .accelerator("<Control><Shift>k")
        .build();
    let m10 = gtk::ShortcutsShortcut::builder()
        .title("Nachricht verschieben")
        .accelerator("<Control><Shift>v")
        .build();
    let m11 = gtk::ShortcutsShortcut::builder()
        .title("Nachricht kopieren")
        .accelerator("<Control><Shift>c")
        .build();
    let m12 = gtk::ShortcutsShortcut::builder()
        .title("Quelltext anzeigen")
        .accelerator("<Control>u")
        .build();
    let m13 = gtk::ShortcutsShortcut::builder()
        .title("Filter anwenden")
        .accelerator("<Control>y")
        .build();

    group_mail.append(&m1);
    group_mail.append(&m2);
    group_mail.append(&m3);
    group_mail.append(&m4);
    group_mail.append(&m5);
    group_mail.append(&m6);
    group_mail.append(&m7);
    group_mail.append(&m8);
    group_mail.append(&m9);
    group_mail.append(&m10);
    group_mail.append(&m11);
    group_mail.append(&m12);
    group_mail.append(&m13);
    section.append(&group_mail);

    // Group 3: Module navigation
    let group_groupware = gtk::ShortcutsGroup::builder()
        .title("Module wechseln")
        .build();

    let g1 = gtk::ShortcutsShortcut::builder()
        .title("Mail")
        .accelerator("<Control>1")
        .build();
    let g2 = gtk::ShortcutsShortcut::builder()
        .title("Kalender")
        .accelerator("<Control>2")
        .build();
    let g3 = gtk::ShortcutsShortcut::builder()
        .title("Kontakte")
        .accelerator("<Control>3")
        .build();
    let g4 = gtk::ShortcutsShortcut::builder()
        .title("Aufgaben")
        .accelerator("<Control>4")
        .build();
    let g5 = gtk::ShortcutsShortcut::builder()
        .title("Notizen")
        .accelerator("<Control>5")
        .build();

    group_groupware.append(&g1);
    group_groupware.append(&g2);
    group_groupware.append(&g3);
    group_groupware.append(&g4);
    group_groupware.append(&g5);
    section.append(&group_groupware);

    window.set_child(Some(&section));
    window.present();
}
