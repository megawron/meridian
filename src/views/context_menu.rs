use gtk::prelude::*;

/// Creates a standard Popover configured as a Libadwaita / GNOME HIG context menu.
pub fn create_context_popover(pointing_to: Option<&gdk::Rectangle>) -> (gtk::Popover, gtk::Box) {
    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .build();
    popover.add_css_class("menu");
    popover.add_css_class("context-menu");
    if let Some(rect) = pointing_to {
        popover.set_pointing_to(Some(rect));
    }

    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    popover.set_child(Some(&menu_box));
    (popover, menu_box)
}

/// Creates a standard Libadwaita-compliant menu item button for context menus.
/// Metrics: min-height 32px, padding 0 12px, border-radius 9px, 16px symbolic icon, standard body font.
pub fn create_context_menu_button(
    icon_name: Option<&str>,
    label_text: &str,
    destructive: bool,
) -> gtk::Button {
    create_context_menu_button_with_shortcut(icon_name, label_text, None, destructive)
}

/// Creates a standard Libadwaita-compliant menu item button with an optional keyboard accelerator label.
pub fn create_context_menu_button_with_shortcut(
    icon_name: Option<&str>,
    label_text: &str,
    shortcut: Option<&str>,
    destructive: bool,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .has_frame(false)
        .halign(gtk::Align::Fill)
        .build();
    btn.add_css_class("flat");
    btn.add_css_class("context-menu-item");
    if destructive {
        btn.add_css_class("destructive-action");
    }

    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(4)
        .margin_end(4)
        .margin_top(2)
        .margin_bottom(2)
        .valign(gtk::Align::Center)
        .build();

    if let Some(icon) = icon_name {
        let img = gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(16)
            .valign(gtk::Align::Center)
            .build();
        if !destructive {
            img.add_css_class("dim-label");
        }
        hbox.append(&img);
    }

    let lbl = gtk::Label::builder()
        .label(label_text)
        .xalign(0.0)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    hbox.append(&lbl);

    if let Some(sc) = shortcut {
        let sc_lbl = gtk::Label::builder()
            .label(sc)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .css_classes(vec!["accelerator", "dim-label"])
            .build();
        hbox.append(&sc_lbl);
    }

    btn.set_child(Some(&hbox));
    btn
}

/// Creates a standard horizontal separator for context menus (margin 6px 0).
pub fn create_context_menu_separator() -> gtk::Separator {
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep.add_css_class("context-menu-separator");
    sep
}
