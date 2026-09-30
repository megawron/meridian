// SPDX-License-Identifier: GPL-3.0-or-later
//
// Small GTK accessibility helpers shared by the application views.

use gtk::prelude::*;

/// Updates the accessible label (screen reader name) for a widget.
pub(crate) fn set_accessible_label<W: IsA<gtk::Widget>>(widget: &W, label: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(label)]);
}

/// Updates the accessible description (extended screen reader help text) for a widget.
pub(crate) fn set_accessible_description<W: IsA<gtk::Widget>>(widget: &W, description: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Description(description)]);
}

/// Sets the AT-SPI accessible role for a widget.
pub(crate) fn set_accessible_role<W: IsA<gtk::Widget>>(widget: &W, role: gtk::AccessibleRole) {
    widget.as_ref().set_accessible_role(role);
}

/// Updates accessible states (e.g. Selected, Expanded, Checked, Busy, Disabled).
pub(crate) fn set_accessible_state<W: IsA<gtk::Widget>>(widget: &W, state: &[gtk::accessible::State]) {
    widget.as_ref().update_state(state);
}

/// Updates accessible relations (e.g. LabelledBy, DescribedBy, Controls).
pub(crate) fn set_accessible_relation<W: IsA<gtk::Widget>>(widget: &W, relation: &[gtk::accessible::Relation]) {
    widget.as_ref().update_relation(relation);
}

/// Announces a live update directly to the screen reader (Orca) via AT-SPI.
pub(crate) fn announce_to_screen_reader<W: IsA<gtk::Widget>>(
    widget: &W,
    message: &str,
    priority: gtk::AccessibleAnnouncementPriority,
) {
    let w = widget.as_ref();
    w.announce(message, priority);
    // Also update accessible description so assistive technologies can read it on inspect
    w.update_property(&[gtk::accessible::Property::Description(message)]);
}

/// Returns whether the GNOME system high contrast mode is currently active.
pub(crate) fn is_high_contrast_active() -> bool {
    adw::StyleManager::default().is_high_contrast()
}

/// Returns the system text scaling factor (e.g. 1.0, 1.25, 1.5) configured by GNOME Large Text.
pub(crate) fn get_system_font_scale() -> f64 {
    if let Some(schema_source) = gio::SettingsSchemaSource::default() {
        if schema_source.lookup("org.gnome.desktop.interface", true).is_some() {
            let settings = gio::Settings::new("org.gnome.desktop.interface");
            let scale = settings.double("text-scaling-factor");
            if scale >= 0.5 && scale <= 4.0 {
                return scale;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accessibility_helpers() {
        if crate::window::gtk_test_main_context().is_none() {
            return;
        }
        let btn = gtk::Button::new();
        set_accessible_label(&btn, "Test Button");
        set_accessible_description(&btn, "Extended Help");
        set_accessible_role(&btn, gtk::AccessibleRole::Button);
        set_accessible_state(&btn, &[gtk::accessible::State::Selected(Some(true))]);
        announce_to_screen_reader(&btn, "Status update", gtk::AccessibleAnnouncementPriority::Medium);

        let scale = get_system_font_scale();
        assert!(scale >= 0.5 && scale <= 4.0);
    }
}

