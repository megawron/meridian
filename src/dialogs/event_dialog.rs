// SPDX-License-Identifier: GPL-3.0-or-later
//
// EventDialog — Dialog for creating and editing calendar events.
//
// Features:
// - Summary / Title entry
// - Location & Description
// - Calendar Source dropdown (Persönlich, Arbeit, etc.)
// - All-day toggle
// - Start and End Date/Time selection

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct EventDialog;

    #[glib::object_subclass]
    impl ObjectSubclass for EventDialog {
        const NAME: &'static str = "EvolutionEventDialog";
        type Type = super::EventDialog;
        type ParentType = adw::Window;
    }

    impl ObjectImpl for EventDialog {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build_ui();
        }
    }

    impl WidgetImpl for EventDialog {}
    impl WindowImpl for EventDialog {}
    impl AdwWindowImpl for EventDialog {}
}

glib::wrapper! {
    pub struct EventDialog(ObjectSubclass<imp::EventDialog>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl EventDialog {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("title", "Neuer Termin")
            .property("modal", true)
            .property("default-width", 560)
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
                    "Calendar event save requested, but the ECal/EDS backend is not connected"
                );
                let toast = adw::Toast::new(
                    "Speichern ist noch nicht verbunden — der Termin bleibt geöffnet",
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

        // Title entry
        let entry_title = gtk::Entry::builder()
            .placeholder_text("Titel des Termins…")
            .css_classes(vec!["title-2"])
            .build();
        content_box.append(&entry_title);

        // Group 1: General Details
        let group_details = adw::PreferencesGroup::builder().title("Details").build();

        let row_calendar = adw::ComboRow::builder()
            .title("Kalender")
            .model(&gtk::StringList::new(&["Kein EDS-Kalender verfügbar"]))
            .build();
        group_details.add(&row_calendar);

        let row_location = adw::EntryRow::builder().title("Ort").build();
        group_details.add(&row_location);

        let row_allday = adw::SwitchRow::builder()
            .title("Ganztägig")
            .active(false)
            .build();
        group_details.add(&row_allday);

        content_box.append(&group_details);

        // Group 2: Time Selection
        let group_time = adw::PreferencesGroup::builder().title("Zeitraum").build();

        let row_start = adw::ActionRow::builder()
            .title("Beginn")
            .subtitle("Noch nicht festgelegt")
            .build();
        let btn_start_time = gtk::Button::builder()
            .label("Startzeit")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .build();
        row_start.add_suffix(&btn_start_time);
        group_time.add(&row_start);

        let row_end = adw::ActionRow::builder()
            .title("Ende")
            .subtitle("Noch nicht festgelegt")
            .build();
        let btn_end_time = gtk::Button::builder()
            .label("Endzeit")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .build();
        row_end.add_suffix(&btn_end_time);
        group_time.add(&row_end);

        content_box.append(&group_time);

        // Description
        let group_desc = adw::PreferencesGroup::builder()
            .title("Beschreibung & Notizen")
            .build();

        let text_desc = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .height_request(80)
            .build();
        group_desc.add(&text_desc);
        content_box.append(&group_desc);

        content_scroll.set_child(Some(&content_box));
        toolbar_view.set_content(Some(&content_scroll));
        self.set_content(Some(&toolbar_view));
    }
}

impl Default for EventDialog {
    fn default() -> Self {
        Self::new()
    }
}
