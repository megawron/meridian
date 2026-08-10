// SPDX-License-Identifier: GPL-3.0-or-later
//
// OutOfOfficeDialog — Abwesenheitsnotiz & Auto-Responder Dialog (Exchange EWS / IMAP).
//
// Features:
// - Status Toggle (Abwesenheitsnotiz aktivieren/deaktivieren)
// - Zeitbereich (Start- & Enddatum)
// - Nachrichten für interne & externe Absender

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct OutOfOfficeDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for OutOfOfficeDialog {
        const NAME: &'static str = "EvolutionOutOfOfficeDialog";
        type Type = super::OutOfOfficeDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for OutOfOfficeDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for OutOfOfficeDialog {}
    impl WindowImpl for OutOfOfficeDialog {}
    impl AdwWindowImpl for OutOfOfficeDialog {}
}

glib::wrapper! {
    pub struct OutOfOfficeDialog(ObjectSubclass<imp::OutOfOfficeDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl OutOfOfficeDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Abwesenheitsnotiz (Auto-Responder)")
            .property("modal", true)
            .property("default-width", 600)
            .property("default-height", 520)
            .build()
    }

    fn build_ui(&self) {
        let toolbar_view = adw::ToolbarView::new();

        let header_bar = adw::HeaderBar::new();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        btn_save.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                tracing::warn!(
                    "Out-of-office save requested, but the account backend is not connected"
                );
                let toast = adw::Toast::new(
                    "Speichern ist noch nicht verbunden — die Abwesenheitsnotiz bleibt geöffnet",
                );
                if let Some(parent) = dialog.transient_for() {
                    if let Ok(window) = parent.downcast::<crate::window::EvolutionWindow>() {
                        window.add_toast(&toast);
                    }
                }
            }
        ));
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let content_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();

        let content_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();

        // Status Group
        let group_status = adw::PreferencesGroup::builder()
            .title("Status & Zeitfenster")
            .build();

        let row_enabled = adw::SwitchRow::builder()
            .title("Abwesenheitsnotiz aktivieren")
            .subtitle("Automatische Antworten auf eingehende E-Mails senden")
            .active(false)
            .build();
        group_status.add(&row_enabled);

        let row_schedule = adw::SwitchRow::builder()
            .title("Nur in folgendem Zeitraum senden")
            .active(true)
            .build();
        group_status.add(&row_schedule);

        let row_start = adw::ActionRow::builder()
            .title("Startdatum")
            .subtitle("Noch nicht festgelegt")
            .build();
        group_status.add(&row_start);

        let row_end = adw::ActionRow::builder()
            .title("Enddatum")
            .subtitle("Noch nicht festgelegt")
            .build();
        group_status.add(&row_end);

        content_box.append(&group_status);

        // Internal Message
        let group_internal = adw::PreferencesGroup::builder()
            .title("Nachricht für Kollegen (Intern)")
            .build();

        let text_internal = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_internal.add(&text_internal);
        content_box.append(&group_internal);

        // External Message
        let group_external = adw::PreferencesGroup::builder()
            .title("Nachricht für externe Absender")
            .build();

        let text_external = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_external.add(&text_external);
        content_box.append(&group_external);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for OutOfOfficeDialog {
    fn default() -> Self {
        Self::new()
    }
}
