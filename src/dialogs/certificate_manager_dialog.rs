// SPDX-License-Identifier: GPL-3.0-or-later
//
// CertificateManagerDialog — Interactive Libadwaita Certificate & Trust Manager.
//
// Features:
// - Full Libadwaita HeaderBar with search filtering and view switcher.
// - 3 tabs (AdwViewStack): "Ihre Zertifikate", "Kontakte", "Zertifizierungsstellen".
// - Boxed lists displaying CN, email, issuer, expiration badges, and trust tags.
// - CertificateDetailsDialog: Subject, Issuer, Validity, SHA-256 Fingerprint, Key Usage, PEM view.
// - CertificateTrustDialog: Trust level selector and purpose toggles (SSL, S/MIME, Code signing).
// - PassphrasePromptDialog: Modal password input with show/hide toggle for .p12 import/export.

use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crate::services::certificate_service::{
    CertCategory, CertExportFormat, CertType, CertificateRecord, CertificateService, TrustLevel,
};

// ─────────────────────────────────────────────────────────────────────────────
// 1. CertificateManagerDialog (Main Window)
// ─────────────────────────────────────────────────────────────────────────────

struct ManagerState {
    search_query: String,
    selected_id: Option<String>,
    active_category: CertCategory,
    list_personal: gtk::ListBox,
    list_contact: gtk::ListBox,
    list_authority: gtk::ListBox,
    btn_details: gtk::Button,
    btn_trust: gtk::Button,
    btn_export: gtk::Button,
    btn_delete: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::Window,
}

pub struct CertificateManagerDialog {
    window: adw::Window,
    state: Rc<RefCell<ManagerState>>,
}

impl CertificateManagerDialog {
    pub fn new<W: IsA<gtk::Window>>(parent: Option<&W>) -> Self {
        let window = adw::Window::builder()
            .title("Zertifikatsverwaltung")
            .modal(true)
            .default_width(780)
            .default_height(600)
            .build();

        if let Some(p) = parent {
            window.set_transient_for(Some(p));
        }

        let toolbar_view = adw::ToolbarView::new();
        let toast_overlay = adw::ToastOverlay::new();

        // ── HeaderBar with ViewSwitcher & Search ──────────────────────────────
        let header_bar = adw::HeaderBar::new();

        let search_toggle = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Zertifikate suchen (Strg+F)")
            .build();
        header_bar.pack_start(&search_toggle);

        let btn_close = gtk::Button::builder()
            .label("Schließen")
            .build();
        header_bar.pack_end(&btn_close);

        // View Stack with 3 Categories
        let view_stack = adw::ViewStack::builder()
            .vexpand(true)
            .hexpand(true)
            .build();

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header_bar.set_title_widget(Some(&view_switcher));
        toolbar_view.add_top_bar(&header_bar);

        // ── Search Bar Revealer ───────────────────────────────────────────────
        let search_bar = gtk::SearchBar::new();
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Zertifikate nach Name, E-Mail, Aussteller oder Fingerabdruck durchsuchen…")
            .hexpand(true)
            .build();
        search_bar.set_child(Some(&search_entry));
        search_bar.bind_property("search-mode-enabled", &search_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        search_bar.connect_entry(&search_entry);
        toolbar_view.add_top_bar(&search_bar);

        // ── Tab 1: Ihre Zertifikate (Personal) ────────────────────────────────
        let (tab_personal, list_personal) = create_category_tab(
            "Zertifikate zur persönlichen Identifizierung, zum Signieren von E-Mails und zur Entschlüsselung eingehender Nachrichten."
        );
        let page_personal = view_stack.add_titled(&tab_personal, Some("personal"), "Ihre Zertifikate");
        page_personal.set_icon_name(Some("credentials-symbolic"));

        // ── Tab 2: Kontakte (Contacts) ────────────────────────────────────────
        let (tab_contact, list_contact) = create_category_tab(
            "Zertifikate von Kontakten und Empfängern zur Verschlüsselung gesendeter Nachrichten."
        );
        let page_contact = view_stack.add_titled(&tab_contact, Some("contacts"), "Kontakte");
        page_contact.set_icon_name(Some("avatar-default-symbolic"));

        // ── Tab 3: Zertifizierungsstellen (Authorities) ───────────────────────
        let (tab_authority, list_authority) = create_category_tab(
            "Zertifizierungsstellen (CAs), denen zur Überprüfung von Website- und E-Mail-Zertifikaten vertraut wird."
        );
        let page_authority = view_stack.add_titled(&tab_authority, Some("authorities"), "Zertifizierungsstellen");
        page_authority.set_icon_name(Some("security-high-symbolic"));

        // ── Bottom Action Bar ─────────────────────────────────────────────────
        let action_bar = gtk::ActionBar::new();

        let btn_import = gtk::Button::builder()
            .label("Importieren…")
            .icon_name("document-open-symbolic")
            .tooltip_text("Zertifikat oder PKCS#12-Schlüsselbund importieren")
            .build();
        action_bar.pack_start(&btn_import);

        let btn_export = gtk::Button::builder()
            .label("Exportieren…")
            .icon_name("document-save-symbolic")
            .sensitive(false)
            .tooltip_text("Ausgewähltes Zertifikat exportieren")
            .build();
        action_bar.pack_start(&btn_export);

        let btn_details = gtk::Button::builder()
            .label("Details anzeigen…")
            .icon_name("dialog-information-symbolic")
            .css_classes(vec!["suggested-action"])
            .sensitive(false)
            .tooltip_text("Vollständige Zertifikatseigenschaften und PEM anzeigen")
            .build();
        action_bar.pack_end(&btn_details);

        let btn_trust = gtk::Button::builder()
            .label("Vertrauen bearbeiten…")
            .icon_name("channel-secure-symbolic")
            .sensitive(false)
            .tooltip_text("Vertrauensstufe und Verwendungszwecke anpassen")
            .build();
        action_bar.pack_end(&btn_trust);

        let btn_delete = gtk::Button::builder()
            .label("Löschen")
            .icon_name("user-trash-symbolic")
            .css_classes(vec!["destructive-action"])
            .sensitive(false)
            .tooltip_text("Ausgewähltes Zertifikat aus dem Speicher entfernen")
            .build();
        action_bar.pack_end(&btn_delete);

        toolbar_view.add_bottom_bar(&action_bar);

        toast_overlay.set_child(Some(&view_stack));
        toolbar_view.set_content(Some(&toast_overlay));
        window.set_content(Some(&toolbar_view));

        let state = Rc::new(RefCell::new(ManagerState {
            search_query: String::new(),
            selected_id: None,
            active_category: CertCategory::Personal,
            list_personal: list_personal.clone(),
            list_contact: list_contact.clone(),
            list_authority: list_authority.clone(),
            btn_details: btn_details.clone(),
            btn_trust: btn_trust.clone(),
            btn_export: btn_export.clone(),
            btn_delete: btn_delete.clone(),
            toast_overlay,
            window: window.clone(),
        }));

        // ── Wire Signals ──────────────────────────────────────────────────────
        let window_weak = window.downgrade();
        btn_close.connect_clicked(move |_| {
            if let Some(win) = window_weak.upgrade() {
                win.close();
            }
        });

        // ViewStack page change listener
        let state_for_page = Rc::clone(&state);
        view_stack.connect_visible_child_name_notify(move |stack| {
            let cat = match stack.visible_child_name().as_deref() {
                Some("contacts") => CertCategory::Contact,
                Some("authorities") => CertCategory::Authority,
                _ => CertCategory::Personal,
            };
            let mut s = state_for_page.borrow_mut();
            s.active_category = cat;
            s.selected_id = None;
            update_action_sensitivity(&s);
        });

        // Search entry filter
        let state_for_search = Rc::clone(&state);
        search_entry.connect_search_changed(move |entry| {
            state_for_search.borrow_mut().search_query = entry.text().trim().to_ascii_lowercase();
            reload_all_lists(&state_for_search);
        });

        // Row selection and activation
        setup_list_signals(&list_personal, CertCategory::Personal, &state);
        setup_list_signals(&list_contact, CertCategory::Contact, &state);
        setup_list_signals(&list_authority, CertCategory::Authority, &state);

        // Action: Details
        let state_for_details = Rc::clone(&state);
        btn_details.connect_clicked(move |_| {
            show_details_dialog_for_selection(&state_for_details);
        });

        // Action: Trust
        let state_for_trust = Rc::clone(&state);
        btn_trust.connect_clicked(move |_| {
            show_trust_dialog_for_selection(&state_for_trust);
        });

        // Action: Delete
        let state_for_delete = Rc::clone(&state);
        btn_delete.connect_clicked(move |_| {
            delete_selected_certificate(&state_for_delete);
        });

        // Action: Import
        let state_for_import = Rc::clone(&state);
        btn_import.connect_clicked(move |_| {
            import_certificate_file(&state_for_import);
        });

        // Action: Export
        let state_for_export = Rc::clone(&state);
        btn_export.connect_clicked(move |_| {
            export_selected_certificate(&state_for_export);
        });

        // Initial Load
        reload_all_lists(&state);

        Self { window, state }
    }

    pub fn window(&self) -> &adw::Window {
        &self.window
    }

    pub fn set_transient_for<W: IsA<gtk::Window>>(&self, parent: Option<&W>) {
        self.window.set_transient_for(parent);
    }

    pub fn connect_close_request<F>(&self, f: F)
    where
        F: Fn(&adw::Window) -> glib::Propagation + 'static,
    {
        self.window.connect_close_request(f);
    }

    pub fn present(&self) {
        self.window.present();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Category Tab Builder & Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn create_category_tab(description: &str) -> (gtk::ScrolledWindow, gtk::ListBox) {
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(18)
        .margin_end(18)
        .margin_top(14)
        .margin_bottom(18)
        .build();

    let desc_label = gtk::Label::builder()
        .label(description)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label", "caption"])
        .build();
    content_box.append(&desc_label);

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(vec!["boxed-list"])
        .vexpand(true)
        .build();
    content_box.append(&list_box);

    scrolled.set_child(Some(&content_box));
    (scrolled, list_box)
}

fn setup_list_signals(list: &gtk::ListBox, _category: CertCategory, state: &Rc<RefCell<ManagerState>>) {
    let state_for_select = Rc::clone(state);
    list.connect_row_selected(move |_list, row| {
        let mut s = state_for_select.borrow_mut();
        if let Some(r) = row {
            let id = r.widget_name().to_string();
            s.selected_id = if id.is_empty() { None } else { Some(id) };
        } else {
            s.selected_id = None;
        }
        update_action_sensitivity(&s);
    });

    let state_for_activate = Rc::clone(state);
    list.connect_row_activated(move |_list, row| {
        let id = row.widget_name().to_string();
        if !id.is_empty() {
            state_for_activate.borrow_mut().selected_id = Some(id);
            show_details_dialog_for_selection(&state_for_activate);
        }
    });
}

fn update_action_sensitivity(s: &ManagerState) {
    let has_selection = s.selected_id.is_some();
    s.btn_details.set_sensitive(has_selection);
    s.btn_trust.set_sensitive(has_selection);
    s.btn_export.set_sensitive(has_selection);
    s.btn_delete.set_sensitive(has_selection);
}

fn reload_all_lists(state: &Rc<RefCell<ManagerState>>) {
    let query = state.borrow().search_query.clone();

    let personal_certs = CertificateService::list_certificates(CertCategory::Personal);
    let contact_certs = CertificateService::list_certificates(CertCategory::Contact);
    let authority_certs = CertificateService::list_certificates(CertCategory::Authority);

    let s = state.borrow();
    populate_list_box(&s.list_personal, &personal_certs, &query, CertCategory::Personal);
    populate_list_box(&s.list_contact, &contact_certs, &query, CertCategory::Contact);
    populate_list_box(&s.list_authority, &authority_certs, &query, CertCategory::Authority);
    update_action_sensitivity(&s);
}

fn populate_list_box(
    list: &gtk::ListBox,
    certs: &[CertificateRecord],
    query: &str,
    category: CertCategory,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let filtered: Vec<_> = certs
        .iter()
        .filter(|c| {
            if query.is_empty() {
                return true;
            }
            c.display_name.to_ascii_lowercase().contains(query)
                || c.subject_cn.to_ascii_lowercase().contains(query)
                || c.issuer_cn.to_ascii_lowercase().contains(query)
                || c.serial_hex.to_ascii_lowercase().contains(query)
                || c.sha256_fingerprint.to_ascii_lowercase().contains(query)
                || c.subject_email.to_ascii_lowercase().contains(query)
        })
        .collect();

    if filtered.is_empty() {
        let placeholder = adw::ActionRow::builder()
            .title(if query.is_empty() {
                "Keine Zertifikate in dieser Kategorie vorhanden"
            } else {
                "Keine Zertifikate stimmen mit der Suche überein"
            })
            .subtitle(if query.is_empty() {
                "Klicken Sie auf »Importieren…«, um Zertifikate hinzuzufügen."
            } else {
                "Prüfen Sie den Suchbegriff oder löschen Sie die Eingabe."
            })
            .build();
        placeholder.set_sensitive(false);
        list.append(&placeholder);
        return;
    }

    for cert in filtered {
        let row = adw::ActionRow::builder()
            .title(&cert.display_name)
            .activatable(true)
            .build();

        // Subtitle: Email + Issuer
        let subtitle = if !cert.subject_email.is_empty() {
            format!("{} · Ausgestellt von: {}", cert.subject_email, cert.issuer_cn)
        } else {
            format!("Ausgestellt von: {}", cert.issuer_cn)
        };
        row.set_subtitle(&subtitle);

        // Leading Icon based on category
        let icon_name = match category {
            CertCategory::Personal => "credentials-symbolic",
            CertCategory::Contact => "avatar-default-symbolic",
            CertCategory::Authority => "security-high-symbolic",
        };
        row.add_prefix(&gtk::Image::from_icon_name(icon_name));

        // Badges container (Trailing Suffix)
        let badge_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();

        // Private Key Indicator
        if cert.has_private_key {
            let key_badge = gtk::Image::builder()
                .icon_name("dialog-password-symbolic")
                .tooltip_text("Privater Schlüssel vorhanden")
                .build();
            badge_box.append(&key_badge);
        }

        // Trust Level Badge
        let (trust_label, trust_class) = match cert.trust_level {
            TrustLevel::AlwaysTrust => ("Immer vertraut", "success"),
            TrustLevel::Trusted | TrustLevel::TrustForSigning | TrustLevel::TrustForEncryption => {
                ("Vertrauenswürdig", "success")
            }
            TrustLevel::Marginal => ("Eingeschränkt", "warning"),
            TrustLevel::Untrusted => ("Nicht vertraut", "error"),
            TrustLevel::NeverTrust => ("Niemals vertraut", "error"),
            TrustLevel::Unknown => ("Unbekannt", "dim-label"),
        };
        let lbl_trust = gtk::Label::builder()
            .label(trust_label)
            .css_classes(vec!["caption", trust_class])
            .build();
        badge_box.append(&lbl_trust);

        // Expiration Badge
        let (exp_label, exp_class) = if cert.is_expired {
            ("Abgelaufen".to_string(), "error")
        } else {
            (format!("Gültig bis {}", cert.not_after), "dim-label")
        };
        let lbl_exp = gtk::Label::builder()
            .label(&exp_label)
            .css_classes(vec!["caption", exp_class])
            .build();
        badge_box.append(&lbl_exp);

        // Arrow indicator
        badge_box.append(&gtk::Image::from_icon_name("go-next-symbolic"));

        row.add_suffix(&badge_box);

        row.set_widget_name(&cert.id);

        list.append(&row);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Dialog Action Triggers
// ─────────────────────────────────────────────────────────────────────────────

fn show_details_dialog_for_selection(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, window) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.window.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let details_dialog = CertificateDetailsDialog::new(&cert);
    details_dialog.set_transient_for(Some(&window));
    details_dialog.present();
}

fn show_trust_dialog_for_selection(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, window) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.window.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let state_for_refresh = Rc::clone(state);
    let cert_id = cert.id.clone();
    let trust_dialog = CertificateTrustDialog::new(&cert, move |new_trust, ssl, email, code_sign| {
        let _ = CertificateService::set_trust_level(&cert_id, new_trust);
        let _ = CertificateService::set_ca_trust(&cert_id, ssl, email, code_sign);
        reload_all_lists(&state_for_refresh);
    });
    trust_dialog.set_transient_for(Some(&window));
    trust_dialog.present();
}

fn delete_selected_certificate(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, window) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.window.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let state_for_delete = Rc::clone(state);
    let cert_id = cert.id.clone();
    let prompt = adw::AlertDialog::new(
        Some("Zertifikat löschen?"),
        Some(&format!(
            "Möchten Sie das Zertifikat »{}« wirklich unwiderruflich entfernen?",
            cert.display_name
        )),
    );
    prompt.add_responses(&[("cancel", "Abbrechen"), ("delete", "Löschen")]);
    prompt.set_response_appearance("delete", adw::ResponseAppearance::Destructive);

    prompt.connect_response(None, move |dialog, response| {
        if response == "delete" {
            let s = state_for_delete.borrow();
            let _ = CertificateService::delete_certificate(&cert_id);
            s.toast_overlay.add_toast(adw::Toast::new("Zertifikat erfolgreich entfernt"));
            drop(s);
            state_for_delete.borrow_mut().selected_id = None;
            reload_all_lists(&state_for_delete);
        }
        dialog.close();
    });
    prompt.present(Some(&window));
}

fn import_certificate_file(state: &Rc<RefCell<ManagerState>>) {
    let window = state.borrow().window.clone();
    let file_dialog = gtk::FileDialog::builder()
        .title("Zertifikat importieren")
        .modal(true)
        .build();

    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Alle Zertifikate (*.crt, *.pem, *.cer, *.p12, *.pfx, *.asc, *.gpg)"));
    filter.add_pattern("*.crt");
    filter.add_pattern("*.pem");
    filter.add_pattern("*.cer");
    filter.add_pattern("*.p12");
    filter.add_pattern("*.pfx");
    filter.add_pattern("*.asc");
    filter.add_pattern("*.gpg");

    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    file_dialog.set_filters(Some(&filters));

    let state_for_result = Rc::clone(state);
    file_dialog.open(Some(&window), None::<&gio::Cancellable>, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else { return };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext == "p12" || ext == "pfx" {
            let state_for_prompt = Rc::clone(&state_for_result);
            let path_clone = path.clone();
            let prompt = PassphrasePromptDialog::new_for_import(&path, move |passphrase| {
                let res = CertificateService::import_file(&path_clone, Some(&passphrase));
                match res {
                    Ok(rec) => {
                        state_for_prompt.borrow().toast_overlay.add_toast(adw::Toast::new(&format!(
                            "Zertifikat »{}« erfolgreich importiert",
                            rec.display_name
                        )));
                        reload_all_lists(&state_for_prompt);
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            });
            prompt.set_transient_for(Some(&state_for_result.borrow().window));
            prompt.present();
        } else {
            let res = CertificateService::import_file(&path, None);
            match res {
                Ok(rec) => {
                    state_for_result.borrow().toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Zertifikat »{}« erfolgreich importiert",
                        rec.display_name
                    )));
                    reload_all_lists(&state_for_result);
                }
                Err(err) => {
                    let alert = adw::AlertDialog::new(
                        Some("Import fehlgeschlagen"),
                        Some(&format!("Das Zertifikat konnte nicht importiert werden:\n\n{err}")),
                    );
                    alert.add_response("ok", "OK");
                    alert.present(Some(&state_for_result.borrow().window));
                }
            }
        }
    });
}

fn export_selected_certificate(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, window) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.window.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let file_dialog = gtk::FileDialog::builder()
        .title("Zertifikat exportieren")
        .modal(true)
        .initial_name(&format!("{}.pem", cert.display_name.replace(' ', "_")))
        .build();

    let state_for_save = Rc::clone(state);
    let cert_clone = cert.clone();
    file_dialog.save(Some(&window), None::<&gio::Cancellable>, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else { return };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext == "p12" || ext == "pfx" {
            let state_for_pass = Rc::clone(&state_for_save);
            let cert_id = cert_clone.id.clone();
            let path_for_pass = path.clone();
            let prompt = PassphrasePromptDialog::new_for_export(move |passphrase| {
                let res = CertificateService::export_file(
                    &cert_id,
                    &path_for_pass,
                    CertExportFormat::Pkcs12,
                    Some(&passphrase),
                );
                match res {
                    Ok(_) => {
                        state_for_pass.borrow().toast_overlay.add_toast(adw::Toast::new(
                            "Zertifikat erfolgreich als PKCS#12 exportiert",
                        ));
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            });
            prompt.set_transient_for(Some(&state_for_save.borrow().window));
            prompt.present();
        } else {
            let res = CertificateService::export_file(
                &cert_clone.id,
                &path,
                CertExportFormat::Pem,
                None,
            );
            if res.is_ok() {
                state_for_save
                    .borrow()
                    .toast_overlay
                    .add_toast(adw::Toast::new("Zertifikat erfolgreich exportiert"));
            } else if let Err(err) = res {
                let alert = adw::AlertDialog::new(
                    Some("Export fehlgeschlagen"),
                    Some(&format!("Fehler beim Exportieren:\n\n{err}")),
                );
                alert.add_response("ok", "OK");
                alert.present(Some(&state_for_save.borrow().window));
            }
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. CertificateDetailsDialog (Property Inspector & PEM Viewer)
// ─────────────────────────────────────────────────────────────────────────────

pub struct CertificateDetailsDialog {
    window: adw::Window,
}

impl CertificateDetailsDialog {
    pub fn new(cert: &CertificateRecord) -> Self {
        let window = adw::Window::builder()
            .title("Zertifikatsdetails")
            .modal(true)
            .default_width(620)
            .default_height(680)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let toast_overlay = adw::ToastOverlay::new();

        let header_bar = adw::HeaderBar::new();
        let title_widget = adw::WindowTitle::builder()
            .title("Zertifikatsdetails")
            .subtitle(&cert.display_name)
            .build();
        header_bar.set_title_widget(Some(&title_widget));

        let btn_close = gtk::Button::builder().label("Schließen").build();
        let win_close = window.downgrade();
        btn_close.connect_clicked(move |_| {
            if let Some(win) = win_close.upgrade() {
                win.close();
            }
        });
        header_bar.pack_end(&btn_close);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();

        // Group 1: Subject (Ausgestellt für)
        let group_subject = adw::PreferencesGroup::builder()
            .title("Ausgestellt für (Inhaber / Subject)")
            .build();
        add_detail_row(&group_subject, "Allgemeiner Name (CN)", &cert.subject_cn, true);
        if !cert.subject_email.is_empty() {
            add_detail_row(&group_subject, "E-Mail-Adresse", &cert.subject_email, true);
        }
        if !cert.subject_org.is_empty() {
            add_detail_row(&group_subject, "Organisation (O)", &cert.subject_org, false);
        }
        if !cert.subject_ou.is_empty() {
            add_detail_row(&group_subject, "Organisationseinheit (OU)", &cert.subject_ou, false);
        }
        page.add(&group_subject);

        // Group 2: Issuer (Ausgestellt durch)
        let group_issuer = adw::PreferencesGroup::builder()
            .title("Ausgestellt durch (Aussteller / Issuer)")
            .build();
        add_detail_row(&group_issuer, "Aussteller-CN", &cert.issuer_cn, true);
        if !cert.issuer_org.is_empty() {
            add_detail_row(&group_issuer, "Aussteller-Organisation", &cert.issuer_org, false);
        }
        page.add(&group_issuer);

        // Group 3: Validity
        let group_validity = adw::PreferencesGroup::builder()
            .title("Gültigkeitszeitraum")
            .build();
        add_detail_row(&group_validity, "Gültig ab", &cert.not_before, false);
        add_detail_row(&group_validity, "Gültig bis", &cert.not_after, false);
        let status_row = adw::ActionRow::builder()
            .title("Status")
            .build();
        let (status_text, status_class) = if cert.is_expired {
            ("Abgelaufen", "error")
        } else {
            ("Gültig", "success")
        };
        let status_badge = gtk::Label::builder()
            .label(status_text)
            .valign(gtk::Align::Center)
            .css_classes(vec!["caption", status_class])
            .build();
        status_row.add_suffix(&status_badge);
        group_validity.add(&status_row);
        page.add(&group_validity);

        // Group 4: Key & Security Identifiers
        let group_key = adw::PreferencesGroup::builder()
            .title("Schlüssel & Sicherheitsmerkmale")
            .build();
        add_detail_row(&group_key, "Seriennummer", &cert.serial_hex, true);
        add_detail_row(&group_key, "SHA-256-Fingerabdruck", &cert.sha256_fingerprint, true);

        let type_str = match cert.cert_type {
            CertType::Smime => "X.509 (S/MIME)",
            CertType::OpenPgp => "OpenPGP",
        };
        add_detail_row(&group_key, "Zertifikatstyp", type_str, false);

        let key_row = adw::ActionRow::builder()
            .title("Privater Schlüssel")
            .subtitle(if cert.has_private_key { "Vorhanden" } else { "Nicht vorhanden" })
            .build();
        if cert.has_private_key {
            key_row.add_prefix(&gtk::Image::from_icon_name("dialog-password-symbolic"));
        }
        group_key.add(&key_row);

        // Key Usage Expander
        let expander_usage = adw::ExpanderRow::builder()
            .title("Verwendungszwecke (Key Usage)")
            .subtitle(cert.key_usage_flags.summary())
            .build();
        add_usage_subrow(&expander_usage, "Digitale Signatur", cert.key_usage_flags.digital_signature);
        add_usage_subrow(&expander_usage, "Nichtabstreitbarkeit (Non-Repudiation)", cert.key_usage_flags.non_repudiation);
        add_usage_subrow(&expander_usage, "Schlüsselverschlüsselung", cert.key_usage_flags.key_encipherment);
        add_usage_subrow(&expander_usage, "Datenverschlüsselung", cert.key_usage_flags.data_encipherment);
        add_usage_subrow(&expander_usage, "Schlüsselvereinbarung (Key Agreement)", cert.key_usage_flags.key_agreement);
        add_usage_subrow(&expander_usage, "Zertifikatsausstellung (KeyCertSign)", cert.key_usage_flags.key_cert_sign);
        add_usage_subrow(&expander_usage, "Sperrlistenprüfung (CRLSign)", cert.key_usage_flags.crl_sign);
        add_usage_subrow(&expander_usage, "E-Mail-Schutz (S/MIME)", cert.key_usage_flags.email_protection);
        add_usage_subrow(&expander_usage, "Server-Authentifizierung (SSL/TLS)", cert.key_usage_flags.server_auth);
        add_usage_subrow(&expander_usage, "Client-Authentifizierung", cert.key_usage_flags.client_auth);
        add_usage_subrow(&expander_usage, "Code-Signierung", cert.key_usage_flags.code_signing);
        group_key.add(&expander_usage);

        page.add(&group_key);

        // Group 5: Raw PEM Data
        let group_pem = adw::PreferencesGroup::builder()
            .title("Rohdaten (PEM-Format)")
            .build();

        let pem_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();

        let text_view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::Char)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        text_view.buffer().set_text(&cert.raw_pem);

        let scrolled_pem = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(140)
            .max_content_height(180)
            .css_classes(vec!["card"])
            .child(&text_view)
            .build();
        pem_box.append(&scrolled_pem);

        let btn_copy_pem = gtk::Button::builder()
            .label("In Zwischenablage kopieren")
            .icon_name("edit-copy-symbolic")
            .halign(gtk::Align::End)
            .build();

        let pem_content = cert.raw_pem.clone();
        let toast_overlay_copy = toast_overlay.clone();
        btn_copy_pem.connect_clicked(move |button| {
            button.display().clipboard().set_text(&pem_content);
            toast_overlay_copy.add_toast(adw::Toast::new("PEM in Zwischenablage kopiert"));
        });
        pem_box.append(&btn_copy_pem);
        group_pem.add(&pem_box);
        page.add(&group_pem);

        toast_overlay.set_child(Some(&page));
        toolbar_view.set_content(Some(&toast_overlay));
        window.set_content(Some(&toolbar_view));

        Self { window }
    }

    pub fn set_transient_for<W: IsA<gtk::Window>>(&self, parent: Option<&W>) {
        self.window.set_transient_for(parent);
    }

    pub fn present(&self) {
        self.window.present();
    }
}

fn add_detail_row(group: &adw::PreferencesGroup, title: &str, value: &str, copyable: bool) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .build();

    if copyable {
        let copy_btn = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(vec!["flat"])
            .tooltip_text("Wert kopieren")
            .build();
        let val_clone = value.to_string();
        copy_btn.connect_clicked(move |button| {
            button.display().clipboard().set_text(&val_clone);
        });
        row.add_suffix(&copy_btn);
    }

    group.add(&row);
}

fn add_usage_subrow(expander: &adw::ExpanderRow, title: &str, enabled: bool) {
    let subrow = adw::ActionRow::builder()
        .title(title)
        .build();
    let badge = gtk::Label::builder()
        .label(if enabled { "Ja" } else { "Nein" })
        .css_classes(vec!["caption", if enabled { "success" } else { "dim-label" }])
        .valign(gtk::Align::Center)
        .build();
    subrow.add_suffix(&badge);
    expander.add_row(&subrow);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. CertificateTrustDialog (Trust Level & Purposes Editor)
// ─────────────────────────────────────────────────────────────────────────────

pub struct CertificateTrustDialog {
    window: adw::Window,
}

impl CertificateTrustDialog {
    pub fn new<F>(cert: &CertificateRecord, on_save: F) -> Self
    where
        F: Fn(TrustLevel, bool, bool, bool) + 'static,
    {
        let window = adw::Window::builder()
            .title("Vertrauensstellung bearbeiten")
            .modal(true)
            .default_width(540)
            .default_height(520)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let btn_cancel = gtk::Button::builder().label("Abbrechen").build();
        let btn_save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();

        // Info Banner
        let group_info = adw::PreferencesGroup::builder()
            .title("Zertifikat")
            .description(&format!(
                "Bearbeiten Sie die Vertrauensregeln für »{}« (Aussteller: {}).",
                cert.display_name, cert.issuer_cn
            ))
            .build();
        page.add(&group_info);

        // Trust Level Combo
        let group_level = adw::PreferencesGroup::builder()
            .title("Vertrauensstufe")
            .build();

        let trust_items = [
            "Immer vertrauen",
            "Vertrauenswürdig (Standard)",
            "Eingeschränkt vertrauenswürdig",
            "Nicht vertrauenswürdig",
            "Niemals vertrauen",
        ];
        let model = gtk::StringList::new(&trust_items);
        let combo_trust = adw::ComboRow::builder()
            .title("Vertrauen")
            .subtitle("Legt fest, wie das Zertifikat in kryptografischen Prüfungen bewertet wird")
            .model(&model)
            .build();

        let initial_index = match cert.trust_level {
            TrustLevel::AlwaysTrust => 0,
            TrustLevel::Trusted | TrustLevel::TrustForSigning | TrustLevel::TrustForEncryption => 1,
            TrustLevel::Marginal => 2,
            TrustLevel::Untrusted => 3,
            TrustLevel::NeverTrust => 4,
            TrustLevel::Unknown => 1,
        };
        combo_trust.set_selected(initial_index);
        group_level.add(&combo_trust);
        page.add(&group_level);

        // Purpose Toggles (CA Usage)
        let group_purposes = adw::PreferencesGroup::builder()
            .title("Verwendungszwecke")
            .description("Geben Sie an, für welche Einsatzbereiche dieser Zertifizierungsstelle vertraut wird:")
            .build();

        let switch_ssl = adw::SwitchRow::builder()
            .title("Websites (SSL/TLS)")
            .subtitle("Dieser Zertifizierungsstelle zum Identifizieren von Websites vertrauen")
            .active(cert.key_usage_flags.server_auth)
            .build();
        group_purposes.add(&switch_ssl);

        let switch_email = adw::SwitchRow::builder()
            .title("E-Mail-Benutzer (S/MIME)")
            .subtitle("Dieser Zertifizierungsstelle zum Identifizieren von E-Mail-Benutzern vertrauen")
            .active(cert.key_usage_flags.email_protection)
            .build();
        group_purposes.add(&switch_email);

        let switch_code = adw::SwitchRow::builder()
            .title("Softwarehersteller (Code-Signatur)")
            .subtitle("Dieser Zertifizierungsstelle zum Identifizieren von Softwareherstellern vertrauen")
            .active(cert.key_usage_flags.code_signing)
            .build();
        group_purposes.add(&switch_code);

        page.add(&group_purposes);
        toolbar_view.set_content(Some(&page));
        window.set_content(Some(&toolbar_view));

        // Wire Cancel
        let win_cancel = window.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(win) = win_cancel.upgrade() {
                win.close();
            }
        });

        // Wire Save
        let win_save = window.downgrade();
        btn_save.connect_clicked(move |_| {
            let selected_trust = match combo_trust.selected() {
                0 => TrustLevel::AlwaysTrust,
                1 => TrustLevel::Trusted,
                2 => TrustLevel::Marginal,
                3 => TrustLevel::Untrusted,
                4 => TrustLevel::NeverTrust,
                _ => TrustLevel::Trusted,
            };
            let ssl = switch_ssl.is_active();
            let email = switch_email.is_active();
            let code = switch_code.is_active();

            on_save(selected_trust, ssl, email, code);

            if let Some(win) = win_save.upgrade() {
                win.close();
            }
        });

        Self { window }
    }

    pub fn set_transient_for<W: IsA<gtk::Window>>(&self, parent: Option<&W>) {
        self.window.set_transient_for(parent);
    }

    pub fn present(&self) {
        self.window.present();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. PassphrasePromptDialog (Modal for Encrypted .p12 Import & Export)
// ─────────────────────────────────────────────────────────────────────────────

pub struct PassphrasePromptDialog {
    window: adw::Window,
}

impl PassphrasePromptDialog {
    pub fn new_for_import<F>(path: &Path, on_submit: F) -> Self
    where
        F: Fn(String) -> Result<(), String> + 'static,
    {
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("Zertifikat");
        let window = adw::Window::builder()
            .title("Passphrase erforderlich")
            .modal(true)
            .default_width(440)
            .default_height(260)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let btn_cancel = gtk::Button::builder().label("Abbrechen").build();
        let btn_unlock = gtk::Button::builder()
            .label("Entschlüsseln")
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_unlock);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("PKCS#12-Schlüsselbund entschlüsseln")
            .description(&format!(
                "Die Datei »{filename}« ist durch eine Passphrase geschützt. Bitte geben Sie das Kennwort ein, um den Import durchzuführen:"
            ))
            .build();

        let row_pass = adw::PasswordEntryRow::builder()
            .title("Passphrase")
            .build();
        group.add(&row_pass);

        let lbl_error = gtk::Label::builder()
            .css_classes(vec!["error", "caption"])
            .visible(false)
            .xalign(0.0)
            .wrap(true)
            .build();
        group.add(&lbl_error);

        page.add(&group);
        toolbar_view.set_content(Some(&page));
        window.set_content(Some(&toolbar_view));

        let win_cancel = window.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(win) = win_cancel.upgrade() {
                win.close();
            }
        });

        let win_unlock = window.downgrade();
        let on_submit_rc = Rc::new(on_submit);
        let perform_unlock = {
            let row_pass = row_pass.clone();
            let lbl_error = lbl_error.clone();
            let on_submit = Rc::clone(&on_submit_rc);
            let win_unlock = win_unlock.clone();
            move || {
                let password = row_pass.text().to_string();
                match on_submit(password) {
                    Ok(_) => {
                        if let Some(win) = win_unlock.upgrade() {
                            win.close();
                        }
                    }
                    Err(err) => {
                        lbl_error.set_label(&format!("Fehler: {err}"));
                        lbl_error.set_visible(true);
                    }
                }
            }
        };

        btn_unlock.connect_clicked({
            let perform = perform_unlock.clone();
            move |_| perform()
        });
        row_pass.connect_entry_activated(move |_| perform_unlock());

        Self { window }
    }

    pub fn new_for_export<F>(on_submit: F) -> Self
    where
        F: Fn(String) -> Result<(), String> + 'static,
    {
        let window = adw::Window::builder()
            .title("Zertifikatsexport schützen")
            .modal(true)
            .default_width(440)
            .default_height(320)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let btn_cancel = gtk::Button::builder().label("Abbrechen").build();
        let btn_export = gtk::Button::builder()
            .label("Exportieren")
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_export);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("PKCS#12-Passphrase festlegen")
            .description("Legen Sie ein Kennwort fest, um den privaten Schlüssel und das Zertifikat im exportierten Archiv zu verschlüsseln:")
            .build();

        let row_pass1 = adw::PasswordEntryRow::builder()
            .title("Neue Passphrase")
            .build();
        group.add(&row_pass1);

        let row_pass2 = adw::PasswordEntryRow::builder()
            .title("Passphrase wiederholen")
            .build();
        group.add(&row_pass2);

        let lbl_error = gtk::Label::builder()
            .css_classes(vec!["error", "caption"])
            .visible(false)
            .xalign(0.0)
            .wrap(true)
            .build();
        group.add(&lbl_error);

        page.add(&group);
        toolbar_view.set_content(Some(&page));
        window.set_content(Some(&toolbar_view));

        let win_cancel = window.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(win) = win_cancel.upgrade() {
                win.close();
            }
        });

        let win_export = window.downgrade();
        let on_submit_rc = Rc::new(on_submit);
        btn_export.connect_clicked(move |_| {
            let p1 = row_pass1.text().to_string();
            let p2 = row_pass2.text().to_string();
            if p1 != p2 {
                lbl_error.set_label("Die Passphrasen stimmen nicht überein.");
                lbl_error.set_visible(true);
                return;
            }
            match on_submit_rc(p1) {
                Ok(_) => {
                    if let Some(win) = win_export.upgrade() {
                        win.close();
                    }
                }
                Err(err) => {
                    lbl_error.set_label(&format!("Fehler: {err}"));
                    lbl_error.set_visible(true);
                }
            }
        });

        Self { window }
    }

    pub fn set_transient_for<W: IsA<gtk::Window>>(&self, parent: Option<&W>) {
        self.window.set_transient_for(parent);
    }

    pub fn present(&self) {
        self.window.present();
    }
}
