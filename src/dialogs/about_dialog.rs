// SPDX-License-Identifier: GPL-3.0-or-later
//
// AboutDialog — Modern AdwAboutDialog displaying version, tech stack, license and credits.

use adw::prelude::*;

pub fn show_about_dialog<W: IsA<gtk::Widget>>(parent: &W) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Evolution GTK4")
        .application_icon("org.gnome.Evolution4")
        .developer_name("Max Jensen & GNOME Community")
        .version("0.1.0")
        .copyright("© 2026 megawron")
        .license_type(gtk::License::Gpl30Only)
        .website("https://gitlab.gnome.org/GNOME/evolution-gtk4")
        .issue_url("https://gitlab.gnome.org/GNOME/evolution-gtk4/-/issues")
        .comments("Technische Vorschau für einen künftigen vollständigen Evolution-Ersatz — GTK4 und Libadwaita.")
        .artists(vec!["megawron"])
        .developers(vec!["megawron"])
        .translator_credits("megawron")
        .build();

    dialog.present(Some(parent));
}
