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

use crate::i18n::gettext;
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
    stack_personal: gtk::Stack,
    stack_contact: gtk::Stack,
    stack_authority: gtk::Stack,
    btn_details: gtk::Button,
    btn_trust: gtk::Button,
    btn_export: gtk::Button,
    btn_delete: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    dialog: adw::Dialog,
}

pub struct CertificateManagerDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
    state: Rc<RefCell<ManagerState>>,
}

impl CertificateManagerDialog {
    pub fn new<W: IsA<gtk::Widget>>(parent: Option<&W>) -> Self {
        let dialog = adw::Dialog::builder()
            .title(&gettext("Certificate Management"))
            .content_width(720)
            .content_height(480)
            .build();

        let stored_parent = RefCell::new(parent.map(|p| p.upcast_ref::<gtk::Widget>().clone()));

        let toolbar_view = adw::ToolbarView::new();
        let toast_overlay = adw::ToastOverlay::new();

        // ── HeaderBar with ViewSwitcher & Search ──────────────────────────────
        let header_bar = adw::HeaderBar::new();
        header_bar.set_margin_start(0);
        header_bar.set_margin_end(0);
        header_bar.set_margin_top(0);
        header_bar.set_margin_bottom(0);

        let btn_add_header = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(&gettext("Import Certificate…"))
            .build();
        header_bar.pack_start(&btn_add_header);

        let search_toggle = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(&gettext("Search certificates (Ctrl+F)"))
            .build();
        header_bar.pack_start(&search_toggle);

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
            .placeholder_text(&gettext("Search certificates by name, email, issuer, or fingerprint…"))
            .hexpand(true)
            .build();
        search_bar.set_child(Some(&search_entry));
        search_bar.bind_property("search-mode-enabled", &search_toggle, "active")
            .bidirectional()
            .sync_create()
            .build();
        search_bar.connect_entry(&search_entry);
        toolbar_view.add_top_bar(&search_bar);

        // ── Tab 1: Personal ──────────────────────────────────────────────────
        let (tab_personal, list_personal, stack_personal) = create_category_tab();
        let page_personal = view_stack.add_titled(&tab_personal, Some("personal"), &gettext("Personal"));
        page_personal.set_icon_name(Some("security-high-symbolic"));

        // ── Tab 2: People ────────────────────────────────────────────────────
        let (tab_contact, list_contact, stack_contact) = create_category_tab();
        let page_contact = view_stack.add_titled(&tab_contact, Some("contacts"), &gettext("People"));
        page_contact.set_icon_name(Some("avatar-default-symbolic"));

        // ── Tab 3: Authorities ───────────────────────────────────────────────
        let (tab_authority, list_authority, stack_authority) = create_category_tab();
        let page_authority = view_stack.add_titled(&tab_authority, Some("authorities"), &gettext("Authorities"));
        page_authority.set_icon_name(Some("channel-secure-symbolic"));

        // ── Bottom Action Bar ─────────────────────────────────────────────────
        let action_bar = gtk::ActionBar::new();
        action_bar.set_margin_start(0);
        action_bar.set_margin_end(0);
        action_bar.set_margin_top(0);
        action_bar.set_margin_bottom(0);

        let btn_export = gtk::Button::builder()
            .label(&gettext("Export…"))
            .icon_name("document-save-symbolic")
            .sensitive(false)
            .tooltip_text(&gettext("Export selected certificate"))
            .build();
        action_bar.pack_start(&btn_export);

        let btn_details = gtk::Button::builder()
            .label(&gettext("View Details…"))
            .icon_name("dialog-information-symbolic")
            .css_classes(vec!["suggested-action"])
            .sensitive(false)
            .tooltip_text(&gettext("View complete certificate properties and PEM"))
            .build();
        action_bar.pack_end(&btn_details);

        let btn_trust = gtk::Button::builder()
            .label(&gettext("Edit Trust…"))
            .icon_name("channel-secure-symbolic")
            .sensitive(false)
            .tooltip_text(&gettext("Adjust trust level and purposes"))
            .build();
        action_bar.pack_end(&btn_trust);

        let btn_delete = gtk::Button::builder()
            .label(&gettext("Delete"))
            .icon_name("user-trash-symbolic")
            .css_classes(vec!["destructive-action"])
            .sensitive(false)
            .tooltip_text(&gettext("Remove selected certificate from storage"))
            .build();
        action_bar.pack_end(&btn_delete);

        toolbar_view.add_bottom_bar(&action_bar);

        toast_overlay.set_child(Some(&view_stack));
        toolbar_view.set_content(Some(&toast_overlay));
        dialog.set_child(Some(&toolbar_view));

        let state = Rc::new(RefCell::new(ManagerState {
            search_query: String::new(),
            selected_id: None,
            active_category: CertCategory::Personal,
            list_personal: list_personal.clone(),
            list_contact: list_contact.clone(),
            list_authority: list_authority.clone(),
            stack_personal,
            stack_contact,
            stack_authority,
            btn_details: btn_details.clone(),
            btn_trust: btn_trust.clone(),
            btn_export: btn_export.clone(),
            btn_delete: btn_delete.clone(),
            toast_overlay,
            dialog: dialog.clone(),
        }));

        // ── Wire Signals ──────────────────────────────────────────────────────
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

        // Action: Import (HeaderBar + button)
        let state_for_import = Rc::clone(&state);
        btn_add_header.connect_clicked(move |_| {
            import_certificate_file(&state_for_import);
        });

        // Action: Export
        let state_for_export = Rc::clone(&state);
        btn_export.connect_clicked(move |_| {
            export_selected_certificate(&state_for_export);
        });

        // Initial Load
        reload_all_lists(&state);

        Self {
            dialog,
            stored_parent,
            state,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn set_transient_for<W: IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        *self.stored_parent.borrow_mut() = parent.map(|p| p.upcast_ref::<gtk::Widget>().clone());
    }

    pub fn connect_close_request<F>(&self, f: F)
    where
        F: Fn(&adw::Dialog) -> glib::Propagation + 'static,
    {
        self.dialog.connect_closed(move |d| {
            let _ = f(d);
        });
    }

    pub fn connect_closed<F>(&self, f: F)
    where
        F: Fn(&adw::Dialog) + 'static,
    {
        self.dialog.connect_closed(f);
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Category Tab Builder & Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn create_category_tab() -> (gtk::Stack, gtk::ListBox, gtk::Stack) {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .build();

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .vexpand(true)
        .hexpand(true)
        .build();

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(vec!["boxed-list"])
        .vexpand(true)
        .hexpand(true)
        .build();
    content_box.append(&list_box);

    scrolled.set_child(Some(&content_box));

    let empty_page = adw::StatusPage::builder()
        .title(&gettext("No Certificates"))
        .css_classes(vec!["compact"])
        .vexpand(true)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();

    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty_page, Some("empty"));
    stack.set_visible_child_name("empty");

    (stack.clone(), list_box, stack)
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

    let (list_personal, list_contact, list_authority, stack_personal, stack_contact, stack_authority) = {
        let s = state.borrow();
        (
            s.list_personal.clone(),
            s.list_contact.clone(),
            s.list_authority.clone(),
            s.stack_personal.clone(),
            s.stack_contact.clone(),
            s.stack_authority.clone(),
        )
    };

    populate_list_box(&stack_personal, &list_personal, &personal_certs, &query, CertCategory::Personal, state);
    populate_list_box(&stack_contact, &list_contact, &contact_certs, &query, CertCategory::Contact, state);
    populate_list_box(&stack_authority, &list_authority, &authority_certs, &query, CertCategory::Authority, state);

    let s = state.borrow();
    update_action_sensitivity(&s);
}

fn populate_list_box(
    stack: &gtk::Stack,
    list: &gtk::ListBox,
    certs: &[CertificateRecord],
    query: &str,
    category: CertCategory,
    _state: &Rc<RefCell<ManagerState>>,
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
        stack.set_visible_child_name("empty");
        return;
    }

    stack.set_visible_child_name("list");

    for cert in filtered {
        let row = adw::ActionRow::builder()
            .title(&cert.display_name)
            .activatable(true)
            .build();

        // Subtitle: Email + Issuer (avoiding redundant display name or self-issued repetition)
        let mut subtitle_parts = Vec::new();
        if !cert.subject_email.is_empty()
            && !cert.subject_email.eq_ignore_ascii_case(&cert.display_name)
        {
            subtitle_parts.push(cert.subject_email.clone());
        }

        if !cert.issuer_cn.is_empty() {
            let is_self_issued = cert.issuer_cn.eq_ignore_ascii_case(&cert.display_name)
                || cert.issuer_cn.eq_ignore_ascii_case(&cert.subject_cn);

            if !is_self_issued {
                subtitle_parts.push(format!("{}: {}", gettext("Issued by"), cert.issuer_cn));
            } else if category == CertCategory::Authority {
                subtitle_parts.push(gettext("Root Certificate Authority"));
            } else if category == CertCategory::Personal {
                subtitle_parts.push(gettext("Self-signed"));
            }
        }

        if !subtitle_parts.is_empty() {
            row.set_subtitle(&subtitle_parts.join(" · "));
        }

        // Leading Icon based on category
        let icon_name = match category {
            CertCategory::Personal => "security-high-symbolic",
            CertCategory::Contact => "avatar-default-symbolic",
            CertCategory::Authority => "channel-secure-symbolic",
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
                .tooltip_text(&gettext("Private key present"))
                .build();
            badge_box.append(&key_badge);
        }

        // Trust Level Badge
        let (trust_label, trust_class) = match cert.trust_level {
            TrustLevel::AlwaysTrust => (gettext("Always trusted"), "success"),
            TrustLevel::Trusted | TrustLevel::TrustForSigning | TrustLevel::TrustForEncryption => {
                (gettext("Trusted"), "success")
            }
            TrustLevel::Marginal => (gettext("Marginal"), "warning"),
            TrustLevel::Untrusted => (gettext("Untrusted"), "error"),
            TrustLevel::NeverTrust => (gettext("Never trusted"), "error"),
            TrustLevel::Unknown => (gettext("Unknown"), "dim-label"),
        };
        let lbl_trust = gtk::Label::builder()
            .label(&trust_label)
            .css_classes(vec!["caption", trust_class])
            .build();
        badge_box.append(&lbl_trust);

        // Expiration Badge
        let (exp_label, exp_class) = if cert.is_expired {
            (gettext("Expired"), "error")
        } else {
            (format!("{} {}", gettext("Expires"), cert.not_after), "dim-label")
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
    let (selected_id, dialog) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.dialog.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let details_dialog = CertificateDetailsDialog::new(&cert);
    details_dialog.set_transient_for(Some(&dialog));
    details_dialog.present();
}

fn show_trust_dialog_for_selection(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, dialog) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.dialog.clone())
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
    trust_dialog.set_transient_for(Some(&dialog));
    trust_dialog.present();
}

fn delete_selected_certificate(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, dialog) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.dialog.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let state_for_delete = Rc::clone(state);
    let cert_id = cert.id.clone();
    let prompt = adw::AlertDialog::new(
        Some(&gettext("Delete Certificate?")),
        Some(&format!(
            "{}: “{}”?",
            gettext("Are you sure you want to permanently delete the certificate"),
            cert.display_name
        )),
    );
    prompt.add_responses(&[("cancel", &gettext("Cancel")), ("delete", &gettext("Delete"))]);
    prompt.set_response_appearance("delete", adw::ResponseAppearance::Destructive);

    prompt.connect_response(None, move |alert, response| {
        if response == "delete" {
            let s = state_for_delete.borrow();
            let _ = CertificateService::delete_certificate(&cert_id);
            s.toast_overlay.add_toast(adw::Toast::new(&gettext("Certificate successfully deleted")));
            drop(s);
            state_for_delete.borrow_mut().selected_id = None;
            reload_all_lists(&state_for_delete);
        }
        alert.close();
    });
    prompt.present(Some(&dialog));
}

fn import_certificate_file(state: &Rc<RefCell<ManagerState>>) {
    let dialog = state.borrow().dialog.clone();
    let file_dialog = gtk::FileDialog::builder()
        .title(&gettext("Import Certificate"))
        .modal(true)
        .build();

    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&gettext("All Certificates (*.crt, *.pem, *.cer, *.p12, *.pfx, *.asc, *.gpg)")));
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
    let parent_win = dialog.root().and_downcast::<gtk::Window>();
    file_dialog.open(parent_win.as_ref(), None::<&gio::Cancellable>, move |result| {
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
                            "{}: “{}”",
                            gettext("Successfully imported certificate"),
                            rec.display_name
                        )));
                        reload_all_lists(&state_for_prompt);
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            });
            prompt.set_transient_for(Some(&state_for_result.borrow().dialog));
            prompt.present();
        } else {
            let res = CertificateService::import_file(&path, None);
            match res {
                Ok(rec) => {
                    state_for_result.borrow().toast_overlay.add_toast(adw::Toast::new(&format!(
                        "{}: “{}”",
                        gettext("Successfully imported certificate"),
                        rec.display_name
                    )));
                    reload_all_lists(&state_for_result);
                }
                Err(err) => {
                    let alert = adw::AlertDialog::new(
                        Some(&gettext("Import Failed")),
                        Some(&format!("{}:\n\n{err}", gettext("The certificate could not be imported"))),
                    );
                    alert.add_response("ok", &gettext("OK"));
                    alert.present(Some(&state_for_result.borrow().dialog));
                }
            }
        }
    });
}

fn export_selected_certificate(state: &Rc<RefCell<ManagerState>>) {
    let (selected_id, dialog) = {
        let s = state.borrow();
        (s.selected_id.clone(), s.dialog.clone())
    };
    let Some(id) = selected_id else { return };
    let Some(cert) = CertificateService::get_certificate(&id) else { return };

    let file_dialog = gtk::FileDialog::builder()
        .title(&gettext("Export Certificate"))
        .modal(true)
        .initial_name(&format!("{}.pem", cert.display_name.replace(' ', "_")))
        .build();

    let state_for_save = Rc::clone(state);
    let cert_clone = cert.clone();
    let parent_win = dialog.root().and_downcast::<gtk::Window>();
    file_dialog.save(parent_win.as_ref(), None::<&gio::Cancellable>, move |result| {
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
                            &gettext("Certificate successfully exported as PKCS#12"),
                        ));
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
            });
            prompt.set_transient_for(Some(&state_for_save.borrow().dialog));
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
                    .add_toast(adw::Toast::new(&gettext("Certificate successfully exported")));
            } else if let Err(err) = res {
                let alert = adw::AlertDialog::new(
                    Some(&gettext("Export Failed")),
                    Some(&format!("{}:\n\n{err}", gettext("Error exporting certificate"))),
                );
                alert.add_response("ok", &gettext("OK"));
                alert.present(Some(&state_for_save.borrow().dialog));
            }
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. CertificateDetailsDialog (Property Inspector & PEM Viewer)
// ─────────────────────────────────────────────────────────────────────────────

pub struct CertificateDetailsDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl CertificateDetailsDialog {
    pub fn new(cert: &CertificateRecord) -> Self {
        let dialog = adw::Dialog::builder()
            .title(&gettext("Certificate Details"))
            .content_width(620)
            .content_height(680)
            .build();

        let stored_parent = RefCell::new(None);

        let toolbar_view = adw::ToolbarView::new();
        let toast_overlay = adw::ToastOverlay::new();

        let header_bar = adw::HeaderBar::new();
        let title_widget = adw::WindowTitle::builder()
            .title(&gettext("Certificate Details"))
            .subtitle(&cert.display_name)
            .build();
        header_bar.set_title_widget(Some(&title_widget));

        let btn_close = gtk::Button::builder().label(&gettext("Close")).build();
        let dlg_close = dialog.downgrade();
        btn_close.connect_clicked(move |_| {
            if let Some(dlg) = dlg_close.upgrade() {
                dlg.close();
            }
        });
        header_bar.pack_end(&btn_close);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();

        // Group 1: Subject
        let group_subject = adw::PreferencesGroup::builder()
            .title(&gettext("Issued to (Subject)"))
            .build();
        add_detail_row(&group_subject, &gettext("Common Name (CN)"), &cert.subject_cn, true);
        if !cert.subject_email.is_empty() {
            add_detail_row(&group_subject, &gettext("Email Address"), &cert.subject_email, true);
        }
        if !cert.subject_org.is_empty() {
            add_detail_row(&group_subject, &gettext("Organization (O)"), &cert.subject_org, false);
        }
        if !cert.subject_ou.is_empty() {
            add_detail_row(&group_subject, &gettext("Organizational Unit (OU)"), &cert.subject_ou, false);
        }
        page.add(&group_subject);

        // Group 2: Issuer
        let group_issuer = adw::PreferencesGroup::builder()
            .title(&gettext("Issued by (Issuer)"))
            .build();
        add_detail_row(&group_issuer, &gettext("Issuer CN"), &cert.issuer_cn, true);
        if !cert.issuer_org.is_empty() {
            add_detail_row(&group_issuer, &gettext("Issuer Organization"), &cert.issuer_org, false);
        }
        page.add(&group_issuer);

        // Group 3: Validity
        let group_validity = adw::PreferencesGroup::builder()
            .title(&gettext("Validity Period"))
            .build();
        add_detail_row(&group_validity, &gettext("Valid From"), &cert.not_before, false);
        add_detail_row(&group_validity, &gettext("Valid To"), &cert.not_after, false);
        let status_row = adw::ActionRow::builder()
            .title(&gettext("Status"))
            .build();
        let (status_text, status_class) = if cert.is_expired {
            (gettext("Expired"), "error")
        } else {
            (gettext("Valid"), "success")
        };
        let status_badge = gtk::Label::builder()
            .label(&status_text)
            .valign(gtk::Align::Center)
            .css_classes(vec!["caption", status_class])
            .build();
        status_row.add_suffix(&status_badge);
        group_validity.add(&status_row);
        page.add(&group_validity);

        // Group 4: Key & Security Identifiers
        let group_key = adw::PreferencesGroup::builder()
            .title(&gettext("Key & Security Features"))
            .build();
        add_detail_row(&group_key, &gettext("Serial Number"), &cert.serial_hex, true);
        add_detail_row(&group_key, &gettext("SHA-256 Fingerprint"), &cert.sha256_fingerprint, true);

        let type_str = match cert.cert_type {
            CertType::Smime => "X.509 (S/MIME)",
            CertType::OpenPgp => "OpenPGP",
        };
        add_detail_row(&group_key, &gettext("Certificate Type"), type_str, false);

        let priv_key_str = if cert.has_private_key {
            gettext("Present")
        } else {
            gettext("Not present")
        };
        let key_row = adw::ActionRow::builder()
            .title(&gettext("Private Key"))
            .subtitle(&priv_key_str)
            .build();
        if cert.has_private_key {
            key_row.add_prefix(&gtk::Image::from_icon_name("dialog-password-symbolic"));
        }
        group_key.add(&key_row);

        // Key Usage Expander
        let expander_usage = adw::ExpanderRow::builder()
            .title(&gettext("Key Usage"))
            .subtitle(cert.key_usage_flags.summary())
            .build();
        add_usage_subrow(&expander_usage, &gettext("Digital Signature"), cert.key_usage_flags.digital_signature);
        add_usage_subrow(&expander_usage, &gettext("Non-Repudiation"), cert.key_usage_flags.non_repudiation);
        add_usage_subrow(&expander_usage, &gettext("Key Encipherment"), cert.key_usage_flags.key_encipherment);
        add_usage_subrow(&expander_usage, &gettext("Data Encipherment"), cert.key_usage_flags.data_encipherment);
        add_usage_subrow(&expander_usage, &gettext("Key Agreement"), cert.key_usage_flags.key_agreement);
        add_usage_subrow(&expander_usage, &gettext("Certificate Signing (KeyCertSign)"), cert.key_usage_flags.key_cert_sign);
        add_usage_subrow(&expander_usage, &gettext("CRL Signing (CRLSign)"), cert.key_usage_flags.crl_sign);
        add_usage_subrow(&expander_usage, &gettext("Email Protection (S/MIME)"), cert.key_usage_flags.email_protection);
        add_usage_subrow(&expander_usage, &gettext("Server Authentication (SSL/TLS)"), cert.key_usage_flags.server_auth);
        add_usage_subrow(&expander_usage, &gettext("Client Authentication"), cert.key_usage_flags.client_auth);
        add_usage_subrow(&expander_usage, &gettext("Code Signing"), cert.key_usage_flags.code_signing);
        group_key.add(&expander_usage);

        page.add(&group_key);

        // Group 5: Raw PEM Data
        let group_pem = adw::PreferencesGroup::builder()
            .title(&gettext("Raw Data (PEM Format)"))
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
            .label(&gettext("Copy to Clipboard"))
            .icon_name("edit-copy-symbolic")
            .halign(gtk::Align::End)
            .build();

        let pem_content = cert.raw_pem.clone();
        let toast_overlay_copy = toast_overlay.clone();
        btn_copy_pem.connect_clicked(move |button| {
            button.display().clipboard().set_text(&pem_content);
            toast_overlay_copy.add_toast(adw::Toast::new(&gettext("PEM copied to clipboard")));
        });
        pem_box.append(&btn_copy_pem);
        group_pem.add(&pem_box);
        page.add(&group_pem);

        toast_overlay.set_child(Some(&page));
        toolbar_view.set_content(Some(&toast_overlay));
        dialog.set_child(Some(&toolbar_view));

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn set_transient_for<W: IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        *self.stored_parent.borrow_mut() = parent.map(|p| p.upcast_ref::<gtk::Widget>().clone());
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
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
            .tooltip_text(&gettext("Copy value"))
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
        .label(&if enabled { gettext("Yes") } else { gettext("No") })
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
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl CertificateTrustDialog {
    pub fn new<F>(cert: &CertificateRecord, on_save: F) -> Self
    where
        F: Fn(TrustLevel, bool, bool, bool) + 'static,
    {
        let dialog = adw::Dialog::builder()
            .title(&gettext("Edit Certificate Trust"))
            .content_width(540)
            .content_height(480)
            .build();

        let stored_parent = RefCell::new(None);

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let title_widget = adw::WindowTitle::builder()
            .title(&gettext("Certificate Trust"))
            .subtitle(&cert.display_name)
            .build();
        header_bar.set_title_widget(Some(&title_widget));

        let btn_cancel = gtk::Button::builder().label(&gettext("Cancel")).build();
        let btn_save = gtk::Button::builder()
            .label(&gettext("Save"))
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_save);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();

        // Trust Level Combo
        let group_level = adw::PreferencesGroup::builder()
            .title(&gettext("Trust Level"))
            .build();

        let trust_item_always = gettext("Always trust");
        let trust_item_trusted = gettext("Trusted (Default)");
        let trust_item_marginal = gettext("Marginally trusted");
        let trust_item_untrusted = gettext("Untrusted");
        let trust_item_never = gettext("Never trust");
        let trust_items = [
            trust_item_always.as_str(),
            trust_item_trusted.as_str(),
            trust_item_marginal.as_str(),
            trust_item_untrusted.as_str(),
            trust_item_never.as_str(),
        ];
        let model = gtk::StringList::new(&trust_items);
        let combo_trust = adw::ComboRow::builder()
            .title(&gettext("Trust"))
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
            .title(&gettext("Key Purposes"))
            .build();

        let switch_ssl = adw::SwitchRow::builder()
            .title(&gettext("Websites (SSL/TLS)"))
            .active(cert.key_usage_flags.server_auth)
            .build();
        group_purposes.add(&switch_ssl);

        let switch_email = adw::SwitchRow::builder()
            .title(&gettext("Email Users (S/MIME)"))
            .active(cert.key_usage_flags.email_protection)
            .build();
        group_purposes.add(&switch_email);

        let switch_code = adw::SwitchRow::builder()
            .title(&gettext("Software Developers (Code Signing)"))
            .active(cert.key_usage_flags.code_signing)
            .build();
        group_purposes.add(&switch_code);

        page.add(&group_purposes);
        toolbar_view.set_content(Some(&page));
        dialog.set_child(Some(&toolbar_view));

        // Wire Cancel
        let dlg_cancel = dialog.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dlg) = dlg_cancel.upgrade() {
                dlg.close();
            }
        });

        // Wire Save
        let dlg_save = dialog.downgrade();
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

            if let Some(dlg) = dlg_save.upgrade() {
                dlg.close();
            }
        });

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn set_transient_for<W: IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        *self.stored_parent.borrow_mut() = parent.map(|p| p.upcast_ref::<gtk::Widget>().clone());
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. PassphrasePromptDialog (Modal for Encrypted .p12 Import & Export)
// ─────────────────────────────────────────────────────────────────────────────

pub struct PassphrasePromptDialog {
    dialog: adw::Dialog,
    stored_parent: RefCell<Option<gtk::Widget>>,
}

impl PassphrasePromptDialog {
    pub fn new_for_import<F>(path: &Path, on_submit: F) -> Self
    where
        F: Fn(String) -> Result<(), String> + 'static,
    {
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("certificate");
        let dialog = adw::Dialog::builder()
            .title(&gettext("Passphrase Required"))
            .content_width(440)
            .content_height(240)
            .build();

        let stored_parent = RefCell::new(None);

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let title_widget = adw::WindowTitle::builder()
            .title(&gettext("Passphrase Required"))
            .subtitle(filename)
            .build();
        header_bar.set_title_widget(Some(&title_widget));

        let btn_cancel = gtk::Button::builder().label(&gettext("Cancel")).build();
        let btn_unlock = gtk::Button::builder()
            .label(&gettext("Unlock"))
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_unlock);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(&gettext("Unlock PKCS#12 Bundle"))
            .build();

        let row_pass = adw::PasswordEntryRow::builder()
            .title(&gettext("Passphrase"))
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
        dialog.set_child(Some(&toolbar_view));

        let dlg_cancel = dialog.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dlg) = dlg_cancel.upgrade() {
                dlg.close();
            }
        });

        let dlg_unlock = dialog.downgrade();
        let on_submit_rc = Rc::new(on_submit);
        let perform_unlock = {
            let row_pass = row_pass.clone();
            let lbl_error = lbl_error.clone();
            let on_submit = Rc::clone(&on_submit_rc);
            let dlg_unlock = dlg_unlock.clone();
            move || {
                let password = row_pass.text().to_string();
                match on_submit(password) {
                    Ok(_) => {
                        if let Some(dlg) = dlg_unlock.upgrade() {
                            dlg.close();
                        }
                    }
                    Err(err) => {
                        lbl_error.set_label(&format!("{}: {err}", gettext("Error")));
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

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn new_for_export<F>(on_submit: F) -> Self
    where
        F: Fn(String) -> Result<(), String> + 'static,
    {
        let dialog = adw::Dialog::builder()
            .title(&gettext("Protect Certificate Export"))
            .content_width(440)
            .content_height(280)
            .build();

        let stored_parent = RefCell::new(None);

        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        let btn_cancel = gtk::Button::builder().label(&gettext("Cancel")).build();
        let btn_export = gtk::Button::builder()
            .label(&gettext("Export"))
            .css_classes(vec!["suggested-action"])
            .build();

        header_bar.pack_start(&btn_cancel);
        header_bar.pack_end(&btn_export);
        toolbar_view.add_top_bar(&header_bar);

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(&gettext("Set PKCS#12 Passphrase"))
            .build();

        let row_pass1 = adw::PasswordEntryRow::builder()
            .title(&gettext("New Passphrase"))
            .build();
        group.add(&row_pass1);

        let row_pass2 = adw::PasswordEntryRow::builder()
            .title(&gettext("Repeat Passphrase"))
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
        dialog.set_child(Some(&toolbar_view));

        let dlg_cancel = dialog.downgrade();
        btn_cancel.connect_clicked(move |_| {
            if let Some(dlg) = dlg_cancel.upgrade() {
                dlg.close();
            }
        });

        let dlg_export = dialog.downgrade();
        let on_submit_rc = Rc::new(on_submit);
        btn_export.connect_clicked(move |_| {
            let p1 = row_pass1.text().to_string();
            let p2 = row_pass2.text().to_string();
            if p1 != p2 {
                lbl_error.set_label(&gettext("The passphrases do not match."));
                lbl_error.set_visible(true);
                return;
            }
            match on_submit_rc(p1) {
                Ok(_) => {
                    if let Some(dlg) = dlg_export.upgrade() {
                        dlg.close();
                    }
                }
                Err(err) => {
                    lbl_error.set_label(&format!("{}: {err}", gettext("Error")));
                    lbl_error.set_visible(true);
                }
            }
        });

        Self {
            dialog,
            stored_parent,
        }
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn set_transient_for<W: IsA<gtk::Widget>>(&self, parent: Option<&W>) {
        *self.stored_parent.borrow_mut() = parent.map(|p| p.upcast_ref::<gtk::Widget>().clone());
    }

    pub fn present(&self) {
        self.dialog.present(self.stored_parent.borrow().as_ref());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{gtk_test_main_context, GTK_TEST_MUTEX};

    #[test]
    fn test_certificate_manager_dialog_metrics_and_tabs() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let mgr = CertificateManagerDialog::new(None::<&gtk::Widget>);
        assert_eq!(mgr.dialog().content_width(), 720);
        assert_eq!(mgr.dialog().content_height(), 480);

        let s = mgr.state.borrow();
        assert_eq!(s.active_category, CertCategory::Personal);
        assert!(s.selected_id.is_none());
        assert!(!s.btn_details.is_sensitive());
        assert!(!s.btn_trust.is_sensitive());
        assert!(!s.btn_export.is_sensitive());
        assert!(!s.btn_delete.is_sensitive());
        assert_eq!(s.stack_personal.visible_child_name().as_deref(), Some("empty"));
    }

    #[test]
    fn test_certificate_trust_dialog_no_explanatory_text() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let cert = CertificateRecord {
            id: "test-trust-id".to_string(),
            display_name: "Test Trust Cert".to_string(),
            category: CertCategory::Personal,
            cert_type: CertType::Smime,
            subject_cn: "Trust CN".to_string(),
            subject_email: "trust@example.com".to_string(),
            subject_org: "Meridian".to_string(),
            subject_ou: String::new(),
            issuer_cn: "Trust CA".to_string(),
            issuer_org: "Meridian CA".to_string(),
            serial_hex: "02".to_string(),
            not_before: "2026-01-01".to_string(),
            not_after: "2027-01-01".to_string(),
            is_expired: false,
            sha256_fingerprint: "11:22:33".to_string(),
            key_usage: Vec::new(),
            key_usage_flags: Default::default(),
            trust_level: TrustLevel::Trusted,
            raw_pem: String::new(),
            cert_path: None,
            private_key_path: None,
            has_private_key: true,
        };

        let trust_dialog = CertificateTrustDialog::new(&cert, |_, _, _, _| {});
        assert_eq!(trust_dialog.dialog().content_width(), 540);
        assert_eq!(trust_dialog.dialog().content_height(), 480);
    }

    #[test]
    fn test_passphrase_prompt_dialogs() {
        let _gtk_lock = GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(_context) = gtk_test_main_context() else {
            return;
        };

        let import_dlg = PassphrasePromptDialog::new_for_import(Path::new("test.p12"), |_| Ok(()));
        assert_eq!(import_dlg.dialog().content_width(), 440);
        assert_eq!(import_dlg.dialog().content_height(), 240);

        let export_dlg = PassphrasePromptDialog::new_for_export(|_| Ok(()));
        assert_eq!(export_dlg.dialog().content_width(), 440);
        assert_eq!(export_dlg.dialog().content_height(), 280);
    }
}
