// SPDX-License-Identifier: GPL-3.0-or-later
//
// AboutDialog — Modern AdwAboutDialog displaying version, tech stack, license and credits.

use adw::prelude::*;

pub fn show_about_dialog<W: IsA<gtk::Widget>>(parent: &W) {
    let comments = crate::i18n::gettext("Modern, fast Mail, Calendar and Groupware Client built with GTK4 and Libadwaita.");
    let dialog = adw::AboutDialog::builder()
        .application_name("Meridian")
        .application_icon("com.martingawron.Meridian")
        .developer_name("megawron")
        .version("0.1.0")
        .copyright("© 2026 megawron")
        .license_type(gtk::License::Gpl30Only)
        .website("https://github.com/megawron/meridian")
        .issue_url("https://github.com/mgawron/evolution-gtk4/issues")
        .comments(&comments)
        .artists(vec!["megawron"])
        .developers(vec!["megawron"])
        .translator_credits("megawron")
        .build();

    dialog.present(Some(parent));
}
