// SPDX-License-Identifier: GPL-3.0-or-later
//
// Small GTK accessibility helpers shared by the application views.

use gtk::prelude::*;

pub(crate) fn set_accessible_label<W: IsA<gtk::Widget>>(widget: &W, label: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(label)]);
}
