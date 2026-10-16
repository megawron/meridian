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

thread_local! {
    static DESKTOP_INTERFACE_SETTINGS: std::cell::RefCell<Option<Option<gio::Settings>>> =
        const { std::cell::RefCell::new(None) };
}

fn with_desktop_interface_settings<R>(f: impl FnOnce(&gio::Settings) -> R) -> Option<R> {
    DESKTOP_INTERFACE_SETTINGS.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            let resolved = gio::SettingsSchemaSource::default()
                .and_then(|source| source.lookup("org.gnome.desktop.interface", true))
                .map(|_| gio::Settings::new("org.gnome.desktop.interface"));
            *borrow = Some(resolved);
        }
        borrow.as_ref().and_then(|opt| opt.as_ref()).map(f)
    })
}

/// Returns the system text scaling factor (e.g. 1.0, 1.25, 1.5) configured by GNOME Large Text.
pub(crate) fn get_system_font_scale() -> f64 {
    if let Some(scale) = with_desktop_interface_settings(|s| s.double("text-scaling-factor")) {
        if (0.5..=4.0).contains(&scale) {
            return scale;
        }
    }
    1.0
}

/// Returns whether system animations are enabled (respects GNOME Reduced Motion / Accessibility).
pub(crate) fn are_animations_enabled() -> bool {
    if let Some(gtk_settings) = gtk::Settings::default() {
        if !gtk_settings.is_gtk_enable_animations() {
            return false;
        }
    }
    if let Some(enabled) = with_desktop_interface_settings(|s| s.boolean("enable-animations")) {
        if !enabled {
            return false;
        }
    }
    true
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

        let anim = are_animations_enabled();
        // In headless tests or standard GNOME environments, this returns a valid boolean
        let _ = anim;
    }
}

