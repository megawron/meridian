// SPDX-License-Identifier: GPL-3.0-or-later
//
// AboutDialog — Modern AdwAboutDialog displaying version, tech stack, license and credits.

use adw::prelude::*;

pub fn show_about_dialog<W: IsA<gtk::Widget>>(parent: &W) {
    let comments = crate::i18n::gettext(
        "Modern, fast Mail, Calendar and Groupware Client built with GTK4 and Libadwaita.",
    );
    let dialog = adw::AboutDialog::builder()
        .application_name("Meridian")
        .application_icon("com.martingawron.Meridian")
        .developer_name("Martin Gawron")
        .version(env!("MERIDIAN_VERSION"))
        .copyright("© 2026 Martin Gawron")
        .license_type(gtk::License::Gpl30)
        .website("https://github.com/megawron/meridian")
        .issue_url("https://github.com/megawron/meridian/issues")
        .comments(&comments)
        .artists(vec!["Martin Gawron https://github.com/megawron"])
        .developers(vec!["Martin Gawron https://github.com/megawron"])
        .translator_credits("Martin Gawron https://github.com/megawron")
        .build();

    dialog.add_acknowledgement_section(
        Some(&crate::i18n::gettext("Built on Evolution")),
        &[
            "The Evolution Project https://wiki.gnome.org/Apps/Evolution",
            "Ximian and the original Evolution authors, including Miguel de Icaza and Nat Friedman",
            "Milan Crha and all Evolution Data Server maintainers and contributors",
        ],
    );
    dialog.add_acknowledgement_section(
        Some(&crate::i18n::gettext("Development tools")),
        &["Developed with the assistance of AI tools"],
    );

    dialog.present(Some(parent));
}
