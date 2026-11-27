// SPDX-License-Identifier: GPL-3.0-or-later
//
// Mail Merge Sheet (AdwDialog) — GNOME-native recipient list & live preview
// dialog for sending personalized emails with `{{placeholder}}` variables.

use adw::prelude::*;
use gtk::gio;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::i18n::gettext;
use crate::services::mail_merge_service::{
    extract_placeholders, parse_recipients_csv, render_template_for_recipient, MailMergeRecipient,
};

/// Present the GNOME-style Mail Merge sheet over the composer window.
///
/// - `initial_recipients`: existing merge recipients (or pre-populated from `To:`).
/// - `subject_template` / `body_preview_template`: current composer subject and plain-text body
///   used to display detected `{{placeholders}}` and render the live per-recipient preview.
/// - `on_insert_placeholder`: optional callback when the user clicks a placeholder chip
///   to insert `{{key}}` into the composer.
/// - `on_apply`: invoked with the confirmed list of valid `MailMergeRecipient`s.
pub fn present_mail_merge_dialog<W, FInsert, FApply>(
    parent: &W,
    initial_recipients: Vec<MailMergeRecipient>,
    subject_template: String,
    body_preview_template: String,
    on_insert_placeholder: FInsert,
    on_apply: FApply,
) where
    W: IsA<gtk::Widget>,
    FInsert: Fn(&str) + 'static,
    FApply: Fn(Vec<MailMergeRecipient>) + 'static,
{
    let dialog = adw::Dialog::builder()
        .title(gettext("Mail Merge"))
        .content_width(680)
        .content_height(640)
        .build();

    let toast_overlay = adw::ToastOverlay::new();
    let toolbar_view = adw::ToolbarView::new();

    let header_bar = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();

    let btn_cancel = gtk::Button::builder().label(gettext("Cancel")).build();
    let btn_apply = gtk::Button::builder()
        .label(gettext("Apply"))
        .css_classes(["suggested-action"])
        .build();
    header_bar.pack_start(&btn_cancel);
    header_bar.pack_end(&btn_apply);
    toolbar_view.add_top_bar(&header_bar);

    let recipients_state: Rc<RefCell<Vec<MailMergeRecipient>>> =
        Rc::new(RefCell::new(initial_recipients));
    let preview_index: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let on_insert_cb: Rc<dyn Fn(&str)> = Rc::new(on_insert_placeholder);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .build();

    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .margin_start(16)
        .margin_end(16)
        .margin_top(12)
        .margin_bottom(16)
        .build();

    let content_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();

    // ── 1. Placeholders Section ─────────────────────────────────────────────
    let placeholders_group = adw::PreferencesGroup::builder()
        .title(gettext("Placeholders"))
        .description(gettext(
            "Click a placeholder to insert it into your message. Extra CSV columns become custom placeholders automatically.",
        ))
        .build();

    let placeholder_chips_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .row_spacing(6)
        .column_spacing(6)
        .homogeneous(false)
        .margin_top(4)
        .build();
    placeholders_group.add(&placeholder_chips_box);
    content_box.append(&placeholders_group);

    // ── 2. Recipients Section ───────────────────────────────────────────────
    let recipients_group = adw::PreferencesGroup::builder()
        .title(gettext("Recipients"))
        .description(gettext(
            "Each recipient receives an individual email with personalized placeholders.",
        ))
        .build();

    let header_actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();

    let btn_import_csv = gtk::Button::builder()
        .label(gettext("Import CSV…"))
        .icon_name("document-open-symbolic")
        .css_classes(["flat"])
        .tooltip_text(gettext("Import recipients from a CSV or TSV file"))
        .build();
    let btn_paste = gtk::MenuButton::builder()
        .label(gettext("Paste…"))
        .icon_name("edit-paste-symbolic")
        .css_classes(["flat"])
        .tooltip_text(gettext("Paste rows from a spreadsheet or email list"))
        .build();
    let btn_add_row = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .css_classes(["flat"])
        .tooltip_text(gettext("Add recipient row"))
        .build();

    // Paste Popover
    let paste_popover = gtk::Popover::new();
    let paste_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(10)
        .margin_bottom(10)
        .width_request(360)
        .build();
    let paste_hint = gtk::Label::builder()
        .label(gettext(
            "Paste CSV, tab-separated spreadsheet rows, or one email per line (e.g. Name, Email):",
        ))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build();
    let paste_view = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(6)
        .right_margin(6)
        .build();
    let paste_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(120)
        .max_content_height(200)
        .child(&paste_view)
        .css_classes(["card"])
        .build();
    let btn_paste_apply = gtk::Button::builder()
        .label(gettext("Add Recipients"))
        .css_classes(["suggested-action"])
        .halign(gtk::Align::End)
        .build();
    paste_box.append(&paste_hint);
    paste_box.append(&paste_scroll);
    paste_box.append(&btn_paste_apply);
    paste_popover.set_child(Some(&paste_box));
    btn_paste.set_popover(Some(&paste_popover));

    header_actions.append(&btn_import_csv);
    header_actions.append(&btn_paste);
    header_actions.append(&btn_add_row);
    recipients_group.set_header_suffix(Some(&header_actions));

    let recipients_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    recipients_group.add(&recipients_list);
    content_box.append(&recipients_group);

    // ── 3. Live Preview Section ─────────────────────────────────────────────
    let preview_group = adw::PreferencesGroup::builder()
        .title(gettext("Preview"))
        .build();

    let preview_nav = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .valign(gtk::Align::Center)
        .build();
    let btn_prev = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["flat", "circular"])
        .tooltip_text(gettext("Previous recipient"))
        .build();
    let lbl_preview_pos = gtk::Label::builder()
        .label("0 / 0")
        .css_classes(["caption", "dim-label", "numeric"])
        .width_chars(7)
        .build();
    let btn_next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["flat", "circular"])
        .tooltip_text(gettext("Next recipient"))
        .build();
    preview_nav.append(&btn_prev);
    preview_nav.append(&lbl_preview_pos);
    preview_nav.append(&btn_next);
    preview_group.set_header_suffix(Some(&preview_nav));

    let preview_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["card"])
        .build();
    let preview_inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(14)
        .margin_end(14)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    let lbl_preview_to = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    let lbl_preview_subject = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    let lbl_preview_body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .lines(6)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["body"])
        .build();
    let lbl_preview_warning = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["caption", "warning"])
        .build();
    preview_inner.append(&lbl_preview_to);
    preview_inner.append(&lbl_preview_subject);
    preview_inner.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    preview_inner.append(&lbl_preview_body);
    preview_inner.append(&lbl_preview_warning);
    preview_card.append(&preview_inner);
    preview_group.add(&preview_card);
    content_box.append(&preview_group);

    clamp.set_child(Some(&content_box));
    scroll.set_child(Some(&clamp));
    toolbar_view.set_content(Some(&scroll));
    toast_overlay.set_child(Some(&toolbar_view));
    dialog.set_child(Some(&toast_overlay));

    // ── Live Refresh Helpers ────────────────────────────────────────────────
    let subject_for_preview = Rc::new(subject_template);
    let body_for_preview = Rc::new(body_preview_template);

    let update_preview: Rc<dyn Fn()> = {
        let state = Rc::clone(&recipients_state);
        let idx_cell = Rc::clone(&preview_index);
        let subj_tpl = Rc::clone(&subject_for_preview);
        let body_tpl = Rc::clone(&body_for_preview);
        let lbl_pos = lbl_preview_pos.clone();
        let lbl_to = lbl_preview_to.clone();
        let lbl_subj = lbl_preview_subject.clone();
        let lbl_body = lbl_preview_body.clone();
        let lbl_warn = lbl_preview_warning.clone();
        let btn_prev = btn_prev.clone();
        let btn_next = btn_next.clone();
        let btn_apply = btn_apply.clone();
        Rc::new(move || {
            let list = state.borrow();
            let valid: Vec<&MailMergeRecipient> = list.iter().filter(|r| r.is_valid()).collect();
            let valid_count = valid.len();
            if valid_count == 0 {
                btn_apply.set_label(&gettext("Clear List"));
                lbl_pos.set_label("0 / 0");
                btn_prev.set_sensitive(false);
                btn_next.set_sensitive(false);
                lbl_to.set_label(&gettext("No valid recipients yet"));
                let no_subject = gettext("(No Subject)");
                lbl_subj.set_label(if subj_tpl.trim().is_empty() {
                    no_subject.as_str()
                } else {
                    subj_tpl.as_str()
                });
                lbl_body.set_label(body_tpl.trim());
                lbl_warn.set_visible(false);
                return;
            }
            btn_apply.set_label(&format!("{} ({})", gettext("Apply"), valid_count));
            let cur = idx_cell.get().min(valid_count.saturating_sub(1));
            idx_cell.set(cur);
            lbl_pos.set_label(&format!("{} / {}", cur + 1, valid_count));
            btn_prev.set_sensitive(cur > 0);
            btn_next.set_sensitive(cur + 1 < valid_count);

            let rcpt = valid[cur];
            let rendered_subj = render_template_for_recipient(&subj_tpl, rcpt, "");
            let rendered_body = render_template_for_recipient(&body_tpl, rcpt, &rendered_subj);
            lbl_to.set_label(&format!(
                "{}: {}",
                gettext("To"),
                rcpt.formatted_to_header()
            ));
            let no_subject = gettext("(No Subject)");
            lbl_subj.set_label(if rendered_subj.trim().is_empty() {
                no_subject.as_str()
            } else {
                rendered_subj.as_str()
            });
            lbl_body.set_label(rendered_body.trim());

            let unresolved = extract_placeholders(&[&rendered_subj, &rendered_body]);
            if unresolved.is_empty() {
                lbl_warn.set_visible(false);
            } else {
                let joined = unresolved
                    .iter()
                    .map(|k| format!("{{{{{k}}}}}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                lbl_warn.set_label(
                    &gettext("Unresolved placeholders for this recipient: {}")
                        .replace("{}", &joined),
                );
                lbl_warn.set_visible(true);
            }
        })
    };

    let rebuild_placeholders: Rc<dyn Fn()> = {
        let chips_box = placeholder_chips_box.clone();
        let state = Rc::clone(&recipients_state);
        let subj_tpl = Rc::clone(&subject_for_preview);
        let body_tpl = Rc::clone(&body_for_preview);
        let on_insert = Rc::clone(&on_insert_cb);
        let toast = toast_overlay.clone();
        Rc::new(move || {
            while let Some(child) = chips_box.first_child() {
                chips_box.remove(&child);
            }
            let mut keys: Vec<String> = vec![
                "first_name".to_string(),
                "name".to_string(),
                "email".to_string(),
            ];
            for detected in extract_placeholders(&[&subj_tpl, &body_tpl]) {
                if !keys.iter().any(|k| k.eq_ignore_ascii_case(&detected)) {
                    keys.push(detected);
                }
            }
            for rcpt in state.borrow().iter() {
                for custom_key in rcpt.custom_fields.keys() {
                    if !keys.iter().any(|k| k.eq_ignore_ascii_case(custom_key)) {
                        keys.push(custom_key.clone());
                    }
                }
            }

            for key in keys {
                let token = format!("{{{{{key}}}}}");
                let btn = gtk::Button::builder()
                    .label(&token)
                    .css_classes(["pill", "flat", "caption"])
                    .tooltip_text(gettext("Click to insert into message"))
                    .build();
                let tok_click = token.clone();
                let on_ins = Rc::clone(&on_insert);
                let toast_c = toast.clone();
                btn.connect_clicked(move |_| {
                    on_ins(&tok_click);
                    toast_c.add_toast(adw::Toast::new(
                        &gettext("Inserted {}").replace("{}", &tok_click),
                    ));
                });
                chips_box.insert(&btn, -1);
            }
        })
    };

    let rebuild_rows_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let rebuild_rows: Rc<dyn Fn()> = {
        let list_box = recipients_list.clone();
        let state = Rc::clone(&recipients_state);
        let update_preview_cb = Rc::clone(&update_preview);
        let rebuild_placeholders_cb = Rc::clone(&rebuild_placeholders);
        let self_slot = Rc::clone(&rebuild_rows_slot);
        Rc::new(move || {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            let snapshot = state.borrow().clone();
            if snapshot.is_empty() {
                let empty_row = adw::ActionRow::builder()
                    .title(gettext("No recipients yet"))
                    .subtitle(gettext(
                        "Click Add, Paste, or Import CSV to add recipients for Mail Merge.",
                    ))
                    .build();
                list_box.append(&empty_row);
            } else {
                for (row_idx, rcpt) in snapshot.iter().enumerate() {
                    let row = gtk::ListBoxRow::builder()
                        .activatable(false)
                        .selectable(false)
                        .build();
                    let hbox = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .margin_start(10)
                        .margin_end(8)
                        .margin_top(6)
                        .margin_bottom(6)
                        .build();

                    let fields_vbox = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(4)
                        .hexpand(true)
                        .build();

                    let top_inputs = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(8)
                        .hexpand(true)
                        .build();

                    let entry_name = gtk::Entry::builder()
                        .text(&rcpt.name)
                        .placeholder_text(gettext("Name"))
                        .hexpand(true)
                        .build();
                    let entry_email = gtk::Entry::builder()
                        .text(&rcpt.email)
                        .placeholder_text(gettext("email@example.com"))
                        .input_purpose(gtk::InputPurpose::Email)
                        .hexpand(true)
                        .build();

                    let state_name = Rc::clone(&state);
                    let upd_name = Rc::clone(&update_preview_cb);
                    entry_name.connect_changed(move |e| {
                        if let Some(item) = state_name.borrow_mut().get_mut(row_idx) {
                            item.name = e.text().to_string();
                        }
                        upd_name();
                    });

                    let state_email = Rc::clone(&state);
                    let upd_email = Rc::clone(&update_preview_cb);
                    entry_email.connect_changed(move |e| {
                        if let Some(item) = state_email.borrow_mut().get_mut(row_idx) {
                            item.email = e.text().trim().to_string();
                        }
                        upd_email();
                    });

                    top_inputs.append(&entry_name);
                    top_inputs.append(&entry_email);
                    fields_vbox.append(&top_inputs);

                    if !rcpt.custom_fields.is_empty() {
                        let summary = rcpt
                            .custom_fields
                            .iter()
                            .filter(|(_, v)| !v.trim().is_empty())
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        if !summary.is_empty() {
                            let custom_lbl = gtk::Label::builder()
                                .label(&summary)
                                .xalign(0.0)
                                .ellipsize(gtk::pango::EllipsizeMode::End)
                                .css_classes(["caption", "dim-label"])
                                .build();
                            fields_vbox.append(&custom_lbl);
                        }
                    }

                    let btn_remove = gtk::Button::builder()
                        .icon_name("user-trash-symbolic")
                        .tooltip_text(gettext("Remove recipient"))
                        .css_classes(["flat", "circular"])
                        .valign(gtk::Align::Center)
                        .build();
                    let state_rm = Rc::clone(&state);
                    let slot_rm = Rc::clone(&self_slot);
                    btn_remove.connect_clicked(move |_| {
                        {
                            let mut list = state_rm.borrow_mut();
                            if row_idx < list.len() {
                                list.remove(row_idx);
                            }
                        }
                        if let Some(cb) = slot_rm.borrow().as_ref().cloned() {
                            cb();
                        }
                    });

                    hbox.append(&fields_vbox);
                    hbox.append(&btn_remove);
                    row.set_child(Some(&hbox));
                    list_box.append(&row);
                }
            }
            rebuild_placeholders_cb();
            update_preview_cb();
        })
    };
    *rebuild_rows_slot.borrow_mut() = Some(Rc::clone(&rebuild_rows));

    // Initial render
    rebuild_rows();

    // ── Signals ─────────────────────────────────────────────────────────────
    {
        let state = Rc::clone(&recipients_state);
        let rebuild = Rc::clone(&rebuild_rows);
        btn_add_row.connect_clicked(move |_| {
            state.borrow_mut().push(MailMergeRecipient::default());
            rebuild();
        });
    }

    {
        let state = Rc::clone(&recipients_state);
        let rebuild = Rc::clone(&rebuild_rows);
        let view = paste_view.clone();
        let pop = paste_popover.clone();
        let toast = toast_overlay.clone();
        btn_paste_apply.connect_clicked(move |_| {
            let buf = view.buffer();
            let text = buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .to_string();
            match parse_recipients_csv(&text) {
                Ok(imported) => {
                    let count = imported.recipients.len();
                    let skipped = imported.skipped_rows;
                    {
                        let mut list = state.borrow_mut();
                        list.retain(|r| r.is_valid());
                        list.extend(imported.recipients);
                    }
                    buf.set_text("");
                    pop.popdown();
                    rebuild();
                    let msg = if skipped > 0 {
                        gettext("Added {} recipients ({} skipped)")
                            .replacen("{}", &count.to_string(), 1)
                            .replacen("{}", &skipped.to_string(), 1)
                    } else {
                        gettext("Added {} recipients").replace("{}", &count.to_string())
                    };
                    toast.add_toast(adw::Toast::new(&msg));
                }
                Err(err) => {
                    toast.add_toast(adw::Toast::new(&err));
                }
            }
        });
    }

    {
        let state = Rc::clone(&recipients_state);
        let rebuild = Rc::clone(&rebuild_rows);
        let toast = toast_overlay.clone();
        let dlg_weak = dialog.downgrade();
        btn_import_csv.connect_clicked(move |_| {
            let parent_win = dlg_weak
                .upgrade()
                .and_then(|d| d.root().and_downcast::<gtk::Window>());
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext("CSV / TSV files (*.csv, *.tsv, *.txt)")));
            filter.add_pattern("*.csv");
            filter.add_pattern("*.tsv");
            filter.add_pattern("*.txt");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);

            let chooser = gtk::FileDialog::builder()
                .title(gettext("Import Recipient List"))
                .modal(true)
                .filters(&filters)
                .build();

            let state_c = Rc::clone(&state);
            let rebuild_c = Rc::clone(&rebuild);
            let toast_c = toast.clone();
            chooser.open(parent_win.as_ref(), None::<&gio::Cancellable>, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        match std::fs::read_to_string(&path) {
                            Ok(content) => match parse_recipients_csv(&content) {
                                Ok(imported) => {
                                    let count = imported.recipients.len();
                                    let skipped = imported.skipped_rows;
                                    {
                                        let mut list = state_c.borrow_mut();
                                        list.retain(|r| r.is_valid());
                                        list.extend(imported.recipients);
                                    }
                                    rebuild_c();
                                    let msg = if skipped > 0 {
                                        gettext("Imported {} recipients ({} skipped)")
                                            .replacen("{}", &count.to_string(), 1)
                                            .replacen("{}", &skipped.to_string(), 1)
                                    } else {
                                        gettext("Imported {} recipients")
                                            .replace("{}", &count.to_string())
                                    };
                                    toast_c.add_toast(adw::Toast::new(&msg));
                                }
                                Err(err) => toast_c.add_toast(adw::Toast::new(&err)),
                            },
                            Err(err) => {
                                toast_c.add_toast(adw::Toast::new(&format!(
                                    "{}: {err}",
                                    gettext("Could not read file")
                                )));
                            }
                        }
                    }
                }
            });
        });
    }

    {
        let idx = Rc::clone(&preview_index);
        let upd = Rc::clone(&update_preview);
        btn_prev.connect_clicked(move |_| {
            let cur = idx.get();
            if cur > 0 {
                idx.set(cur - 1);
                upd();
            }
        });
    }
    {
        let idx = Rc::clone(&preview_index);
        let upd = Rc::clone(&update_preview);
        btn_next.connect_clicked(move |_| {
            idx.set(idx.get().saturating_add(1));
            upd();
        });
    }

    {
        let dlg_cancel = dialog.clone();
        btn_cancel.connect_clicked(move |_| {
            dlg_cancel.close();
        });
    }

    {
        let state = Rc::clone(&recipients_state);
        let dlg_apply = dialog.clone();
        btn_apply.connect_clicked(move |_| {
            let valid: Vec<MailMergeRecipient> = state
                .borrow()
                .iter()
                .filter(|r| r.is_valid())
                .cloned()
                .collect();
            on_apply(valid);
            dlg_apply.close();
        });
    }

    dialog.present(Some(parent));
}
