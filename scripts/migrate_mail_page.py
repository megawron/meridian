#!/usr/bin/env python3
"""
Migrate src/views/mail_page.rs to canonical English + gettext internationalization.
"""
import sys
import re

def migrate(content: str) -> str:
    # 1. format_month_short_de -> remove unused function and update test
    # Find fn format_month_short_de
    content = re.sub(
        r'fn format_month_short_de\(month: u32\) -> &\'static str \{[\s\S]*?_ => "",\s*\}\s*\}',
        r'// format_month_short handles localized month abbreviations via gettext',
        content
    )

    # 2. mail_row_status_icon
    content = content.replace(
        '''fn mail_row_status_icon(
    is_unread: bool,
    is_replied: bool,
    is_forwarded: bool,
) -> (&'static str, &'static str) {
    if is_replied && is_forwarded {
        ("mail-replied-symbolic", "Beantwortet und weitergeleitet")
    } else if is_replied {
        ("mail-replied-symbolic", "Beantwortet")
    } else if is_forwarded {
        ("mail-forwarded-symbolic", "Weitergeleitet")
    } else if is_unread {
        ("media-record-symbolic", "Ungelesen")
    } else {
        ("", "Gelesen")
    }
}''',
        '''fn mail_row_status_icon(
    is_unread: bool,
    is_replied: bool,
    is_forwarded: bool,
) -> (&'static str, String) {
    if is_replied && is_forwarded {
        ("mail-replied-symbolic", gettext("Replied and forwarded"))
    } else if is_replied {
        ("mail-replied-symbolic", gettext("Replied"))
    } else if is_forwarded {
        ("mail-forwarded-symbolic", gettext("Forwarded"))
    } else if is_unread {
        ("media-record-symbolic", gettext("Unread"))
    } else {
        ("", gettext("Read"))
    }
}'''
    )

    # 3. mail_row_attachment_info
    content = content.replace(
        '''fn mail_row_attachment_info(
    attachments: &[crate::models::mail_message::AttachmentInfo],
) -> Option<(&'static str, &'static str)> {
    if attachments.is_empty() {
        return None;
    }
    for att in attachments {
        let mime = att.mime_type.to_ascii_lowercase();
        let filename = att.filename.to_ascii_lowercase();
        if mime.contains("calendar") || filename.ends_with(".ics") {
            return Some(("x-office-calendar-symbolic", "Termineinladung (.ics)"));
        }
        if mime.contains("vcard") || filename.ends_with(".vcf") {
            return Some(("x-office-address-book-symbolic", "Kontaktkarte (.vcf)"));
        }
    }
    Some(("mail-attachment-symbolic", "Anhang"))
}''',
        '''fn mail_row_attachment_info(
    attachments: &[crate::models::mail_message::AttachmentInfo],
) -> Option<(&'static str, String)> {
    if attachments.is_empty() {
        return None;
    }
    for att in attachments {
        let mime = att.mime_type.to_ascii_lowercase();
        let filename = att.filename.to_ascii_lowercase();
        if mime.contains("calendar") || filename.ends_with(".ics") {
            return Some(("x-office-calendar-symbolic", gettext("Calendar invitation (.ics)")));
        }
        if mime.contains("vcard") || filename.ends_with(".vcf") {
            return Some(("x-office-address-book-symbolic", gettext("Contact card (.vcf)")));
        }
    }
    Some(("mail-attachment-symbolic", gettext("Attachment")))
}'''
    )

    # Update usages of status.set_tooltip_text(Some(tooltip))
    content = content.replace(
        'status.set_tooltip_text(Some(tooltip));',
        'status.set_tooltip_text(Some(&tooltip));'
    )
    content = content.replace(
        'attachment.set_tooltip_text(Some(tooltip));',
        'attachment.set_tooltip_text(Some(&tooltip));'
    )

    # 4. Subject display and empty snippet heuristics
    content = content.replace(
        '''    let subject_display = if clean_subject.is_empty() {
        "(Kein Betreff)".to_string()
    } else {
        clean_subject
    };''',
        '''    let subject_display = if clean_subject.is_empty() {
        gettext("(No Subject)")
    } else {
        clean_subject
    };'''
    )

    content = content.replace(
        '''    let has_meaningful_snip = !clean_snip.is_empty()
        && clean_snip != "(Keine Nachricht)"
        && clean_snip != "Keine Nachricht"
        && clean_snip != "<p>(Keine Nachricht)</p>";''',
        '''    let has_meaningful_snip = !clean_snip.is_empty()
        && clean_snip != "(Keine Nachricht)"
        && clean_snip != "Keine Nachricht"
        && clean_snip != "<p>(Keine Nachricht)</p>"
        && clean_snip != "(No message)"
        && clean_snip != "No message"
        && clean_snip != "<p>(No message)</p>";'''
    )

    # 5. normal_header_display_name
    content = content.replace(
        '''fn normal_header_display_name(name: &str) -> &str {
    match name.to_ascii_lowercase().as_str() {
        "from" => "Von",
        "reply-to" => "Antwort an",
        "to" => "An",
        "cc" => "Cc",
        "bcc" => "Bcc",
        "subject" => "Betreff",
        "date" => "Datum",
        "newsgroups" => "Newsgroups",
        "face" => "Face",
        "x-evolution-mailer" => "Mailer",
        _ => name,
    }
}''',
        '''fn normal_header_display_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "from" => gettext("From"),
        "reply-to" => gettext("Reply-To"),
        "to" => gettext("To"),
        "cc" => gettext("Cc"),
        "bcc" => gettext("Bcc"),
        "subject" => gettext("Subject"),
        "date" => gettext("Date"),
        "newsgroups" => gettext("Newsgroups"),
        "face" => gettext("Face"),
        "x-evolution-mailer" => gettext("Mailer"),
        _ => name.to_string(),
    }
}'''
    )

    content = content.replace(
        '''                selected.push((
                    normal_header_display_name(configured_name).to_string(),
                    value,
                ));''',
        '''                selected.push((
                    normal_header_display_name(configured_name),
                    value,
                ));'''
    )

    content = content.replace(
        '''        if name == "Face" {
            let face = crate::services::vcard_service::decode_base64(value)
                .map(glib::Bytes::from_owned)
                .and_then(|bytes| gtk::gdk::Texture::from_bytes(&bytes).ok());
            if let Some(texture) = face {
                let avatar = adw::Avatar::builder()
                    .size(48)
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Center)
                    .build();
                avatar.set_custom_image(Some(&texture));
                grid.attach(&avatar, 1, row as i32, 1, 1);
            } else {
                let invalid = gtk::Label::builder()
                    .label("Absenderbild konnte nicht angezeigt werden")
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption", "dim-label"])
                    .build();
                grid.attach(&invalid, 1, row as i32, 1, 1);
            }
            continue;
        }
        let mut classes = vec!["mail-normal-header-value"];
        if name == "Betreff" {
            classes.push("title-4");
        }''',
        '''        if name == "Face" {
            let face = crate::services::vcard_service::decode_base64(value)
                .map(glib::Bytes::from_owned)
                .and_then(|bytes| gtk::gdk::Texture::from_bytes(&bytes).ok());
            if let Some(texture) = face {
                let avatar = adw::Avatar::builder()
                    .size(48)
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Center)
                    .build();
                avatar.set_custom_image(Some(&texture));
                grid.attach(&avatar, 1, row as i32, 1, 1);
            } else {
                let invalid = gtk::Label::builder()
                    .label(&gettext("Sender picture could not be displayed"))
                    .halign(gtk::Align::Start)
                    .css_classes(vec!["caption", "dim-label"])
                    .build();
                grid.attach(&invalid, 1, row as i32, 1, 1);
            }
            continue;
        }
        let mut classes = vec!["mail-normal-header-value"];
        if name == "Betreff" || name == "Subject" || name == &gettext("Subject") {
            classes.push("title-4");
        }'''
    )

    # 6. displayed_message_search_status
    content = content.replace(
        '''fn displayed_message_search_status(result: &MessageSearchResult) -> String {
    match result.status {
        MessageSearchStatus::EmptyQuery => String::new(),
        MessageSearchStatus::NoDisplayedMessage => "Keine Nachricht angezeigt".to_string(),
        MessageSearchStatus::Pending => "Suche …".to_string(),
        MessageSearchStatus::Found | MessageSearchStatus::NotFound => {
            format!("Treffer: {}", result.match_count)
        }
    }
}''',
        '''fn displayed_message_search_status(result: &MessageSearchResult) -> String {
    match result.status {
        MessageSearchStatus::EmptyQuery => String::new(),
        MessageSearchStatus::NoDisplayedMessage => gettext("No message displayed"),
        MessageSearchStatus::Pending => gettext("Searching…"),
        MessageSearchStatus::Found | MessageSearchStatus::NotFound => {
            format!("{}: {}", gettext("Matches"), result.match_count)
        }
    }
}'''
    )

    # 7. export_filename stem
    content = content.replace(
        '''    let stem = if sanitized.is_empty() {
        "nachricht"
    } else {
        sanitized
    };''',
        '''    let stem = if sanitized.is_empty() {
        "message"
    } else {
        sanitized
    };'''
    )

    # 8. local_folder_properties_snapshot
    content = content.replace(
        '''    let (folder_name, count_key) = match selection.folder_id.as_str() {
        "inbox" => ("Posteingang".to_string(), "inbox"),
        "sent" => ("Gesendet".to_string(), "sent"),
        "drafts" => ("Entwürfe".to_string(), "drafts"),
        "trash" => ("Papierkorb".to_string(), "trash"),
        "archive" => ("Archiv".to_string(), "archive"),
        "spam" => ("Spam".to_string(), "junk"),''',
        '''    let (folder_name, count_key) = match selection.folder_id.as_str() {
        "inbox" => (gettext("Inbox"), "inbox"),
        "sent" => (gettext("Sent"), "sent"),
        "drafts" => (gettext("Drafts"), "drafts"),
        "trash" => (gettext("Trash"), "trash"),
        "archive" => (gettext("Archive"), "archive"),
        "spam" => (gettext("Junk"), "junk"),'''
    )

    content = content.replace(
        '''    Some(crate::dialogs::FolderPropertiesSnapshot {
        folder_path: format!("Lokale Ordner / {folder_name}"),
        folder_name,
        account_name: "Lokale Ordner".to_string(),''',
        '''    let local_folders_label = gettext("Local Folders");
    Some(crate::dialogs::FolderPropertiesSnapshot {
        folder_path: format!("{local_folders_label} / {folder_name}"),
        folder_name,
        account_name: local_folders_label,'''
    )

    # 9. mail_message_location_label
    content = content.replace(
        '''    let folder_name = match folder_id {
        "junk" => "Spam".to_string(),
        _ => MailPage::local_folder_sort_label(folder_id),
    };
    format!("Lokale Ordner / {folder_name}")''',
        '''    let folder_name = match folder_id {
        "junk" => gettext("Junk"),
        _ => MailPage::local_folder_sort_label(folder_id),
    };
    format!("{} / {folder_name}", gettext("Local Folders"))'''
    )

    # 10. MESSAGE_CONTEXT_ACTIONS
    old_actions = '''const MESSAGE_CONTEXT_ACTIONS: &[(&str, &str)] = &[
    ("Antworten", "win.reply"),
    ("Allen antworten", "win.reply-all"),
    ("An Liste antworten", "win.reply-list"),
    ("Alternativ antworten …", "win.reply-alternative"),
    ("Weiterleiten", "win.forward"),
    ("Inline weiterleiten", "win.forward-inline"),
    ("Zitiert weiterleiten", "win.forward-quoted"),
    ("Als Anhang weiterleiten", "win.forward-attached"),
    ("Als EML speichern …", "win.export"),
    ("Drucken …", "win.mail-print"),
    ("Druckvorschau …", "win.mail-print-preview"),
    ("Nachrichtenquelle anzeigen", "win.view-message-source"),
    ("Nachrichtenlink kopieren", "win.copy-message-link"),
    ("Zum enthaltenen Ordner", "win.goto-containing-folder"),
    (
        "Absender zum Adressbuch hinzufügen",
        "win.add-sender-contact",
    ),
    ("Als neue Nachricht bearbeiten", "win.edit-as-new"),
    ("Als gelesen markieren", "win.mark-read"),
    ("Als ungelesen markieren", "win.mark-unread"),
    ("Markierung umschalten", "win.toggle-flag"),
    ("Wichtigkeitsmarkierung umschalten", "win.toggle-important"),
    ("Labels bearbeiten …", "win.edit-labels"),
    ("In Ordner verschieben …", "win.move-message"),
    ("In Ordner kopieren …", "win.copy-message"),
    ("Archivieren", "win.archive"),
    ("Als Spam markieren", "win.mark-junk"),
    ("Als Nicht-Spam markieren", "win.mark-not-junk"),
    ("Löschen", "win.delete"),
    ("Löschen rückgängig machen", "win.undelete"),
];'''

    new_actions = '''const MESSAGE_CONTEXT_ACTIONS: &[(&str, &str)] = &[
    ("Reply", "win.reply"),
    ("Reply to All", "win.reply-all"),
    ("Reply to List", "win.reply-list"),
    ("Reply Alternative…", "win.reply-alternative"),
    ("Forward", "win.forward"),
    ("Forward Inline", "win.forward-inline"),
    ("Forward Quoted", "win.forward-quoted"),
    ("Forward as Attachment", "win.forward-attached"),
    ("Save as EML…", "win.export"),
    ("Print…", "win.mail-print"),
    ("Print Preview…", "win.mail-print-preview"),
    ("View Message Source", "win.view-message-source"),
    ("Copy Message Link", "win.copy-message-link"),
    ("Go to Containing Folder", "win.goto-containing-folder"),
    (
        "Add Sender to Address Book",
        "win.add-sender-contact",
    ),
    ("Edit as New Message", "win.edit-as-new"),
    ("Mark as Read", "win.mark-read"),
    ("Mark as Unread", "win.mark-unread"),
    ("Toggle Flag", "win.toggle-flag"),
    ("Toggle Important", "win.toggle-important"),
    ("Edit Labels…", "win.edit-labels"),
    ("Move to Folder…", "win.move-message"),
    ("Copy to Folder…", "win.copy-message"),
    ("Archive", "win.archive"),
    ("Mark as Junk", "win.mark-junk"),
    ("Mark as Not Junk", "win.mark-not-junk"),
    ("Delete", "win.delete"),
    ("Undelete", "win.undelete"),
];'''

    content = content.replace(old_actions, new_actions)

    content = content.replace(
        '''    fn message_context_menu() -> gio::Menu {
        let menu = gio::Menu::new();
        for (label, action) in MESSAGE_CONTEXT_ACTIONS {
            menu.append(Some(label), Some(action));
        }
        menu
    }''',
        '''    fn message_context_menu() -> gio::Menu {
        let menu = gio::Menu::new();
        for (label, action) in MESSAGE_CONTEXT_ACTIONS {
            let localized = gettext(label);
            menu.append(Some(&localized), Some(action));
        }
        menu
    }'''
    )

    # 11. format_camel_status and loader fallback messages
    content = content.replace(
        '''            Err(graph_error) => MailPageLoadResult::Error(format!(
                "Mailkonto-Synchronisation nicht verfügbar ({error}); Online-Suche ebenfalls nicht verfügbar ({graph_error})"
            )),''',
        '''            Err(graph_error) => MailPageLoadResult::Error(
                gettext("Mail account synchronization unavailable ({error}); online search also unavailable ({graph_error})")
                    .replace("{error}", &error.to_string())
                    .replace("{graph_error}", &graph_error.to_string())
            ),'''
    )

    content = content.replace(
        '''            MailPageLoadResult::Local {
                reason: "Keine aktiven Mailkonten; lokaler Evolution-Speicher".to_string(),''',
        '''            MailPageLoadResult::Local {
                reason: gettext("No active mail accounts; local Evolution storage"),'''
    )

    content = content.replace(
        '''            MailPageLoadResult::Local {
                reason: format!(
                    "Mailkonto-Synchronisation nicht verfügbar ({error}); lokaler Evolution-Speicher"
                ),''',
        '''            MailPageLoadResult::Local {
                reason: gettext("Mail account synchronization unavailable ({error}); local Evolution storage")
                    .replace("{error}", &error.to_string()),'''
    )

    old_camel_status = '''fn format_camel_status(camel: &CamelMailLoad) -> String {
    let backend_label = if camel.search_query.is_some() {
        "Graph-Suche"
    } else if camel.is_graph {
        "Microsoft Graph"
    } else {
        "Mailkonto"
    };
    let account_label = if camel.configured_sources == 1 {
        "Konto"
    } else {
        "Konten"
    };
    let message_label = if camel.messages.len() == 1 {
        "Nachricht"
    } else {
        "Nachrichten"
    };
    let sync_status = if camel.failed_sources == 0 {
        format!(
            "{} {account_label} synchronisiert",
            camel.successful_sources
        )
    } else if camel.successful_sources > 0 {
        format!(
            "{}/{} {account_label} synchronisiert, {} mit Fehler",
            camel.successful_sources, camel.configured_sources, camel.failed_sources
        )
    } else {
        format!(
            "{} {account_label} nicht synchronisiert",
            camel.failed_sources
        )
    };
    let warning = camel
        .warning
        .as_deref()
        .map(|warning| format!(" · Hinweis: {warning}"))
        .unwrap_or_default();
    let folder_warning = (camel.configured_sources > 0
        && camel.successful_sources > 0
        && !camel.folders.iter().any(|folder| folder.is_selectable))
    .then_some(" · Keine lesbaren Mailordner gefunden")
    .unwrap_or_default();

    format!(
        "{backend_label}: {sync_status}, {} {message_label} geladen{warning}{folder_warning}",
        camel.messages.len(),
    )
}'''

    new_camel_status = '''fn format_camel_status(camel: &CamelMailLoad) -> String {
    let backend_label = if camel.search_query.is_some() {
        gettext("Graph Search")
    } else if camel.is_graph {
        "Microsoft Graph".to_string()
    } else {
        gettext("Mail Account")
    };
    let sync_status = if camel.failed_sources == 0 {
        ngettext(
            "{} account synchronized",
            "{} accounts synchronized",
            camel.successful_sources as u32,
        )
        .replace("{}", &camel.successful_sources.to_string())
    } else if camel.successful_sources > 0 {
        gettext("{}/{} accounts synchronized, {} with errors")
            .replace("{}/{}", &format!("{}/{}", camel.successful_sources, camel.configured_sources))
            .replace("{}", &camel.failed_sources.to_string())
    } else {
        ngettext(
            "{} account not synchronized",
            "{} accounts not synchronized",
            camel.failed_sources as u32,
        )
        .replace("{}", &camel.failed_sources.to_string())
    };
    let message_status = ngettext(
        "{} message loaded",
        "{} messages loaded",
        camel.messages.len() as u32,
    )
    .replace("{}", &camel.messages.len().to_string());
    let warning = camel
        .warning
        .as_deref()
        .map(|warning| format!(" · {}: {warning}", gettext("Note")))
        .unwrap_or_default();
    let folder_warning = (camel.configured_sources > 0
        && camel.successful_sources > 0
        && !camel.folders.iter().any(|folder| folder.is_selectable))
    .then(|| format!(" · {}", gettext("No readable mail folders found")))
    .unwrap_or_default();

    format!(
        "{backend_label}: {sync_status}, {message_status}{warning}{folder_warning}"
    )
}'''

    content = content.replace(old_camel_status, new_camel_status)

    # 12. View message headers & toasts (lines 5870-6155)
    content = content.replace(
        '''                    let text = if subj.trim().is_empty() {
                        "(Kein Betreff)"
                    } else {
                        &subj
                    };
                    subject_label.set_text(text);''',
        '''                    let no_subj = gettext("(No Subject)");
                    let text = if subj.trim().is_empty() {
                        &no_subj
                    } else {
                        &subj
                    };
                    subject_label.set_text(text);'''
    )

    content = content.replace(
        '''                                "Die Nachrichtenquelle enthält keine Kopfzeilen".to_string()''',
        '''                                gettext("The message source contains no headers")'''
    )

    content = content.replace(
        '''                    page.show_toast(&format!("Originale Kopfzeilen nicht verfügbar: {error}"));''',
        '''                    page.show_toast(&gettext("Original headers not available: {error}").replace("{error}", &error));'''
    )

    content = content.replace(
        '''            self.show_toast("Kein Fenster für die erweiterte Suche verfügbar");''',
        '''            self.show_toast(&gettext("No window available for advanced search"));'''
    )

    content = content.replace(
        '''            self.show_toast("Diese Nachricht enthält keine List-Post-Adresse");''',
        '''            self.show_toast(&gettext("This message contains no List-Post header"));'''
    )

    # 13. Alternative reply dialog
    content = content.replace(
        '''        let dialog = adw::AlertDialog::builder()
            .heading("Alternativ antworten")
            .body("Empfänger und Stil gelten nur für diese Antwort.")
            .extra_child(&content)
            .default_response("reply")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "Abbrechen"), ("reply", "Antworten")]);''',
        '''        let str_heading = gettext("Reply Alternative");
        let str_body = gettext("Recipient and style apply only to this reply.");
        let dialog = adw::AlertDialog::builder()
            .heading(&str_heading)
            .body(&str_body)
            .extra_child(&content)
            .default_response("reply")
            .close_response("cancel")
            .build();
        let cancel_label = gettext("Cancel");
        let reply_label = gettext("Reply");
        dialog.add_responses(&[("cancel", &cancel_label), ("reply", &reply_label)]);'''
    )

    content = content.replace(
        '''                page.show_toast("Antwortempfänger ist nicht mehr verfügbar");''',
        '''                page.show_toast(&gettext("Reply recipient is no longer available"));'''
    )

    content = content.replace(
        '''                            .ok_or_else(|| "List-Post-Adresse fehlt".to_string())?;''',
        '''                            .ok_or_else(|| gettext("List-Post address is missing"))?;'''
    )

    content = content.replace(
        '''                        page.show_toast("Die List-Post-Adresse ist nicht mehr verfügbar");''',
        '''                        page.show_toast(&gettext("The List-Post address is no longer available"));'''
    )

    # 14. Edit as new, prepare attachment, print
    content = content.replace(
        '''                self.show_toast(&format!(
                    "Nachricht kann nicht vollständig bearbeitet werden: {error}"
                ));''',
        '''                self.show_toast(&gettext("Message cannot be fully edited: {error}").replace("{error}", &error.to_string()));'''
    )

    content = content.replace(
        '''        self.show_toast("Originalnachricht wird für den Anhang geladen …");''',
        '''        self.show_toast(&gettext("Loading original message for attachment…"));'''
    )

    content = content.replace(
        '''                    page.show_toast(&format!(
                        "Originalnachricht konnte nicht angehängt werden: {error}"
                    ));''',
        '''                    page.show_toast(&gettext("Original message could not be attached: {error}").replace("{error}", &error.to_string()));'''
    )

    content = content.replace(
        '''            self.show_toast("Ein Druckvorgang läuft bereits");''',
        '''            self.show_toast(&gettext("A print operation is already in progress"));'''
    )

    content = content.replace(
        '''            self.set_print_state(
                MailPrintJobState::Error,
                "Drucken nicht möglich: keine Nachricht ausgewählt",
            );''',
        '''            self.set_print_state(
                MailPrintJobState::Error,
                &gettext("Printing not possible: no message selected"),
            );'''
    )

    content = content.replace(
        '''        self.set_print_state(
            MailPrintJobState::Preparing,
            if action.is_preview() {
                "Druckvorschau wird vorbereitet …"
            } else {
                "Druck wird vorbereitet …"
            },
        );''',
        '''        let prep_preview = gettext("Preparing print preview…");
        let prep_print = gettext("Preparing print…");
        self.set_print_state(
            MailPrintJobState::Preparing,
            if action.is_preview() {
                &prep_preview
            } else {
                &prep_print
            },
        );'''
    )

    content = content.replace(
        '''            self.finish_print_error(
                generation,
                format!("Druckvorbereitung konnte nicht gestartet werden: {error}"),
            );''',
        '''            self.finish_print_error(
                generation,
                gettext("Could not start print preparation: {error}").replace("{error}", &error.to_string()),
            );'''
    )

    content = content.replace(
        '''                    page.set_print_state(
                        MailPrintJobState::Previewing,
                        "Druckvorschau wird geöffnet …",
                    );''',
        '''                    page.set_print_state(
                        MailPrintJobState::Previewing,
                        &gettext("Opening print preview…"),
                    );'''
    )

    content = content.replace(
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            "Druckvorschau geöffnet",
                        );''',
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            &gettext("Print preview opened"),
                        );'''
    )

    content = content.replace(
        '''                    page.finish_print_state(
                        generation,
                        MailPrintJobState::Cancelled,
                        "Druckvorbereitung abgebrochen",
                    );''',
        '''                    page.finish_print_state(
                        generation,
                        MailPrintJobState::Cancelled,
                        &gettext("Print preparation cancelled"),
                    );'''
    )

    content = content.replace(
        '''        self.set_print_state(MailPrintJobState::Cancelled, "Druckvorgang abgebrochen");''',
        '''        self.set_print_state(MailPrintJobState::Cancelled, &gettext("Print operation cancelled"));'''
    )

    content = content.replace(
        '''        self.set_print_state(
            MailPrintJobState::Error,
            &format!("Drucken nicht möglich: {error}"),
        );
        self.show_toast(&format!("Drucken nicht möglich: {error}"));''',
        '''        let err_msg = gettext("Printing not possible: {error}").replace("{error}", &error);
        self.set_print_state(
            MailPrintJobState::Error,
            &err_msg,
        );
        self.show_toast(&err_msg);'''
    )

    content = content.replace(
        '''.ok_or_else(|| "Kein Fenster für die Druckvorschau verfügbar".to_string())?;''',
        '''.ok_or_else(|| gettext("No window available for print preview"))?;'''
    )

    content = content.replace(
        '''        let preview_window = gtk::Window::builder()
            .title("Druckvorschau")
            .default_width(900)
            .default_height(700)
            .transient_for(&parent)
            .build();
        let header = gtk::HeaderBar::new();
        let close_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Druckvorschau schließen")
            .build();
        set_accessible_label(&close_button, "Druckvorschau schließen");''',
        '''        let preview_title = gettext("Print Preview");
        let preview_window = gtk::Window::builder()
            .title(&preview_title)
            .default_width(900)
            .default_height(700)
            .transient_for(&parent)
            .build();
        let header = gtk::HeaderBar::new();
        let close_tip = gettext("Close Print Preview");
        let close_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(&close_tip)
            .build();
        set_accessible_label(&close_button, &close_tip);'''
    )

    content = content.replace(
        '''        self.set_print_state(MailPrintJobState::Printing, "Druckdialog wird geöffnet …");''',
        '''        self.set_print_state(MailPrintJobState::Printing, &gettext("Opening print dialog…"));'''
    )

    content = content.replace(
        '''                self.finish_print_error(generation, "Kein Fenster für den Druckdialog verfügbar");''',
        '''                self.finish_print_error(generation, gettext("No window available for print dialog"));'''
    )

    content = content.replace(
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            "Druckauftrag wurde an das System übergeben",
                        );''',
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Idle,
                            &gettext("Print job sent to system"),
                        );'''
    )

    content = content.replace(
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Cancelled,
                            "Druck abgebrochen",
                        );''',
        '''                        page.finish_print_state(
                            generation,
                            MailPrintJobState::Cancelled,
                            &gettext("Print cancelled"),
                        );'''
    )

    # 15. Export message EML
    content = content.replace(
        '''        let file_dialog = gtk::FileDialog::builder()
            .title("E-Mail als EML speichern")
            .accept_label("Speichern")
            .initial_name(&filename)
            .modal(true)
            .build();''',
        '''        let file_dialog = gtk::FileDialog::builder()
            .title(&gettext("Save Email as EML"))
            .accept_label(&gettext("Save"))
            .initial_name(&filename)
            .modal(true)
            .build();'''
    )

    content = content.replace(
        '''        filter.set_name(Some("EML-Dateien (*.eml)"));''',
        '''        filter.set_name(Some(&gettext("EML files (*.eml)")));'''
    )

    content = content.replace(
        '''                            page.show_toast(&format!("Speicherdialog-Fehler: {error}"));''',
        '''                            page.show_toast(&gettext("Save dialog error: {error}").replace("{error}", &error.to_string()));'''
    )

    # 16. Message source window
    content = content.replace(
        '''            self.show_toast("Kein Fenster für die Nachrichtenquelle verfügbar");''',
        '''            self.show_toast(&gettext("No window available for message source"));'''
    )

    content = content.replace(
        '''                Err("Abruf abgebrochen".to_string())''',
        '''                Err(gettext("Fetch cancelled"))'''
    )

    content = content.replace(
        '''                                Some("Der Nachrichtenspeicher besitzt keine abrufbare originale Rohquelle. Angezeigt wird eine verlustarme RFC822-Rekonstruktion der geladenen Nachricht.".to_string()),''',
        '''                                Some(gettext("The message store has no retrievable original raw source. A low-loss RFC822 reconstruction of the loaded message is shown.")),'''
    )

    content = content.replace(
        '''                            Some("Die originale Nachrichtenquelle konnte nicht geladen werden. Angezeigt wird eine verlustarme RFC822-Rekonstruktion.".to_string()),''',
        '''                            Some(gettext("The original message source could not be loaded. A low-loss RFC822 reconstruction is shown.")),'''
    )

    content = content.replace(
        '''        let window = adw::Window::builder()
            .title(format!("Nachrichtenquelle — {subject}"))
            .transient_for(parent)
            .modal(false)
            .default_width(900)
            .default_height(700)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Gesamte Nachrichtenquelle kopieren")
            .sensitive(false)
            .build();
        set_accessible_label(&copy_button, "Gesamte Nachrichtenquelle kopieren");''',
        '''        let title = format!("{} — {subject}", gettext("Message Source"));
        let window = adw::Window::builder()
            .title(&title)
            .transient_for(parent)
            .modal(false)
            .default_width(900)
            .default_height(700)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let copy_tip = gettext("Copy entire message source");
        let copy_button = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text(&copy_tip)
            .sensitive(false)
            .build();
        set_accessible_label(&copy_button, &copy_tip);'''
    )

    # 17. Local folder dialog
    content = content.replace(
        '''            self.show_toast("Kein Fenster für die lokalen Ordner verfügbar");''',
        '''            self.show_toast(&gettext("No window available for local folders"));'''
    )

    content = content.replace(
        '''        let entry = gtk::Entry::builder()
            .text(&initial_name)
            .placeholder_text("z. B. Projekte/2026")
            .hexpand(true)
            .activates_default(true)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .build();
        let hint = gtk::Label::builder()
            .label("Unterordner können mit / getrennt werden. Die importierten Maildateien bleiben unverändert.")
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&entry);
        content.append(&hint);

        let heading = if is_rename {
            "Lokalen Ordner umbenennen"
        } else {
            "Lokalen Ordner erstellen"
        };
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body("Der Ordner wird ausschließlich im app-eigenen lokalen Mailbestand angelegt.")
            .extra_child(&content)
            .default_response("save")
            .focus_widget(&entry)
            .build();
        dialog.add_responses(&[("cancel", "Abbrechen"), ("save", "Speichern")]);''',
        '''        let entry = gtk::Entry::builder()
            .text(&initial_name)
            .placeholder_text(&gettext("e.g. Projects/2026"))
            .hexpand(true)
            .activates_default(true)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .build();
        let hint = gtk::Label::builder()
            .label(&gettext("Subfolders can be separated with /. Imported mail files remain unchanged."))
            .wrap(true)
            .halign(gtk::Align::Start)
            .css_classes(vec!["dim-label"])
            .build();
        content.append(&entry);
        content.append(&hint);

        let heading = if is_rename {
            gettext("Rename Local Folder")
        } else {
            gettext("Create Local Folder")
        };
        let body = gettext("The folder is created exclusively in the app's local mail storage.");
        let dialog = adw::AlertDialog::builder()
            .heading(&heading)
            .body(&body)
            .extra_child(&content)
            .default_response("save")
            .focus_widget(&entry)
            .build();
        let cancel_label = gettext("Cancel");
        let save_label = gettext("Save");
        dialog.add_responses(&[("cancel", &cancel_label), ("save", &save_label)]);'''
    )

    content = content.replace(
        '''                        page.show_toast(&format!(
                            "Lokaler Ordner konnte nicht gespeichert werden: {error}"
                        ));''',
        '''                        page.show_toast(&gettext("Local folder could not be saved: {error}").replace("{error}", &error.to_string()));'''
    )

    # 18. Folder sort order
    content = content.replace(
        '''                .unwrap_or_else(|| "Mailkonto".to_string());''',
        '''                .unwrap_or_else(|| gettext("Mail Account"));'''
    )

    content = content.replace(
        '''            return Some(FolderSortOrderLevel {
                title: format!("Reihenfolge in „{parent_label}“"),
                items,
            });''',
        '''            return Some(FolderSortOrderLevel {
                title: gettext("Order in “{parent_label}”").replace("{parent_label}", &parent_label),
                items,
            });'''
    )

    content = content.replace(
        '''            .unwrap_or_else(|| "Lokale Ordner".to_string());
        Some(FolderSortOrderLevel {
            title: format!("Reihenfolge in „{parent_label}“"),
            items,
        })''',
        '''            .unwrap_or_else(|| gettext("Local Folders"));
        Some(FolderSortOrderLevel {
            title: gettext("Order in “{parent_label}”").replace("{parent_label}", &parent_label),
            items,
        })'''
    )

    content = content.replace(
        '''            self.show_toast("Für diesen Ordner gibt es keine sortierbare Ebene");''',
        '''            self.show_toast(&gettext("There is no sortable level for this folder"));'''
    )

    content = content.replace(
        '''            self.show_toast("Diese Ebene enthält weniger als zwei Ordner");''',
        '''            self.show_toast(&gettext("This level contains fewer than two folders"));'''
    )

    content = content.replace(
        '''            self.show_toast("Kein Fenster für die Ordnersortierung verfügbar");''',
        '''            self.show_toast(&gettext("No window available for folder sorting"));'''
    )

    content = content.replace(
        '''            self.show_toast("Ordnersortierung konnte nicht gespeichert werden");''',
        '''            self.show_toast(&gettext("Folder sort order could not be saved"));'''
    )

    content = content.replace(
        '''        self.show_toast(&format!("Ordner „{}“ neu angeordnet", dragged_name));''',
        '''        self.show_toast(&gettext("Folder “{name}” reordered").replace("{name}", dragged_name));'''
    )

    content = content.replace(
        '''        self.show_toast(&format!(
            "Ordner „{}“ in „{}“ verschoben",
            dragged_name, target_name
        ));''',
        '''        self.show_toast(&gettext("Folder “{dragged}” moved to “{target}”").replace("{dragged}", dragged_name).replace("{target}", target_name));'''
    )

    # 19. folder_context_menu
    content = content.replace(
        '''        folder_section.append(Some("Neuer Unterordner..."), Some("win.new-folder"));
        folder_section.append(Some("Ordner umbenennen..."), Some("win.rename-folder"));
        folder_section.append(Some("Ordner löschen..."), Some("win.delete-folder"));
        folder_section.append(Some("Ordner aktualisieren"), Some("win.refresh-folder"));
        folder_menu.append_section(None, &folder_section);

        let action_section = gio::Menu::new();
        action_section.append(
            Some("Alle als gelesen markieren"),
            Some("win.mark-all-read"),
        );
        folder_menu.append_section(None, &action_section);

        let props_section = gio::Menu::new();
        props_section.append(Some("Eigenschaften..."), Some("win.folder-properties"));''',
        '''        let str_new_folder = gettext("New Subfolder…");
        let str_rename = gettext("Rename Folder…");
        let str_delete = gettext("Delete Folder…");
        let str_refresh = gettext("Refresh Folder");
        let str_mark_read = gettext("Mark All as Read");
        let str_props = gettext("Properties…");

        folder_section.append(Some(&str_new_folder), Some("win.new-folder"));
        folder_section.append(Some(&str_rename), Some("win.rename-folder"));
        folder_section.append(Some(&str_delete), Some("win.delete-folder"));
        folder_section.append(Some(&str_refresh), Some("win.refresh-folder"));
        folder_menu.append_section(None, &folder_section);

        let action_section = gio::Menu::new();
        action_section.append(
            Some(&str_mark_read),
            Some("win.mark-all-read"),
        );
        folder_menu.append_section(None, &action_section);

        let props_section = gio::Menu::new();
        props_section.append(Some(&str_props), Some("win.folder-properties"));'''
    )

    # 20. Remote / synchronized folder actions
    content = content.replace(
        '''        self.execute_mail_batch(commands, "Konversation löschen")?;''',
        '''        self.execute_mail_batch(commands, &gettext("Delete conversation"))?;'''
    )

    content = content.replace(
        '''            self.show_toast(
                "Für diese Quelle ist das Erstellen von Camel-Ordnern nicht verfügbar.",
            );''',
        '''            self.show_toast(
                &gettext("Creating Camel folders is not available for this source."),
            );'''
    )

    content = content.replace(
        '''            self.show_toast("Dieser synchronisierte Ordner kann nicht umbenannt werden.");''',
        '''            self.show_toast(&gettext("This synchronized folder cannot be renamed."));'''
    )

    content = content.replace(
        '''            self.show_toast(
                "Standard-Ordner wie Posteingang oder Papierkorb können nicht umbenannt werden.",
            );''',
        '''            self.show_toast(
                &gettext("Default folders such as Inbox or Trash cannot be renamed."),
            );'''
    )

    content = content.replace(
        '''            let heading = format!("Ordner „{}“ löschen?", folder_name);
            let body =
                "Der lokale Ordner und seine Zuordnung werden entfernt. Der Ordner muss leer sein.";
            if let Some(root) = self.root().and_downcast::<gtk::Window>() {
                crate::dialogs::run_destructive_action(
                    &root,
                    &heading,
                    body,
                    "Löschen",
                    move || {
                        let result = crate::services::MailStoreService::new()
                            .delete_local_folder(&folder_id);
                        if let Some(page) = page.upgrade() {
                            match result {
                                Ok(()) => {
                                    page.show_toast(&format!("Ordner „{}“ gelöscht", folder_name));
                                    page.refresh_mail();
                                }
                                Err(error) => {
                                    page.show_toast(&format!(
                                        "Ordner konnte nicht gelöscht werden: {error}"
                                    ));
                                }
                            }
                        }
                    },
                );
            }''',
        '''            let heading = gettext("Delete folder “{folder_name}”?").replace("{folder_name}", &folder_name);
            let body = gettext("The local folder and its assignment will be removed. The folder must be empty.");
            let delete_label = gettext("Delete");
            if let Some(root) = self.root().and_downcast::<gtk::Window>() {
                crate::dialogs::run_destructive_action(
                    &root,
                    &heading,
                    &body,
                    &delete_label,
                    move || {
                        let result = crate::services::MailStoreService::new()
                            .delete_local_folder(&folder_id);
                        if let Some(page) = page.upgrade() {
                            match result {
                                Ok(()) => {
                                    page.show_toast(&gettext("Folder “{folder_name}” deleted").replace("{folder_name}", &folder_name));
                                    page.refresh_mail();
                                }
                                Err(error) => {
                                    page.show_toast(&gettext("Folder could not be deleted: {error}").replace("{error}", &error.to_string()));
                                }
                            }
                        }
                    },
                );
            }'''
    )

    content = content.replace(
        '''            crate::dialogs::run_destructive_action(
                &root,
                &format!("Ordner „{display_name}“ löschen?"),
                "Der Ordner wird beim Mailanbieter gelöscht. Diese Aktion kann nicht rückgängig gemacht werden.",
                "Löschen",
                move || {''',
        '''            let heading = gettext("Delete folder “{display_name}”?").replace("{display_name}", &display_name);
            let body = gettext("The folder will be deleted from the mail provider. This action cannot be undone.");
            let delete_label = gettext("Delete");
            crate::dialogs::run_destructive_action(
                &root,
                &heading,
                &body,
                &delete_label,
                move || {'''
    )

    content = content.replace(
        '''            self.show_toast("Dieser synchronisierte Ordner kann nicht gelöscht werden.");''',
        '''            self.show_toast(&gettext("This synchronized folder cannot be deleted."));'''
    )

    content = content.replace(
        '''            self.show_toast("Standard-Ordner können nicht gelöscht werden.");''',
        '''            self.show_toast(&gettext("Default folders cannot be deleted."));'''
    )

    content = content.replace(
        '''            self.show_toast("Dieser synchronisierte Ordner kann nicht aktualisiert werden.");''',
        '''            self.show_toast(&gettext("This synchronized folder cannot be refreshed."));'''
    )

    content = content.replace(
        '''        let entry = gtk::Entry::builder()
            .text(if matches!(kind, CamelFolderMutationKind::Rename) {
                current_name
            } else {
                ""
            })
            .placeholder_text("Ordnername")
            .activates_default(true)
            .hexpand(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading(if matches!(kind, CamelFolderMutationKind::Rename) {
                "Synchronisierten Ordner umbenennen"
            } else {
                "Unterordner erstellen"
            })
            .body("Die Änderung wird vom Mailanbieter ausgeführt und erst nach dessen Bestätigung angezeigt.")
            .extra_child(&entry)
            .default_response("apply")
            .focus_widget(&entry)
            .build();
        dialog.add_responses(&[("cancel", "Abbrechen"), ("apply", "Speichern")]);''',
        '''        let entry = gtk::Entry::builder()
            .text(if matches!(kind, CamelFolderMutationKind::Rename) {
                current_name
            } else {
                ""
            })
            .placeholder_text(&gettext("Folder Name"))
            .activates_default(true)
            .hexpand(true)
            .build();
        let heading = if matches!(kind, CamelFolderMutationKind::Rename) {
            gettext("Rename Synchronized Folder")
        } else {
            gettext("Create Subfolder")
        };
        let body = gettext("The change will be executed by the mail provider and displayed once confirmed.");
        let dialog = adw::AlertDialog::builder()
            .heading(&heading)
            .body(&body)
            .extra_child(&entry)
            .default_response("apply")
            .focus_widget(&entry)
            .build();
        let cancel_label = gettext("Cancel");
        let apply_label = gettext("Save");
        dialog.add_responses(&[("cancel", &cancel_label), ("apply", &apply_label)]);'''
    )

    content = content.replace(
        '''                    page.show_toast("Der Ordnername muss ein einzelner, nicht leerer Name sein.");''',
        '''                    page.show_toast(&gettext("The folder name must be a single, non-empty name."));'''
    )

    content = content.replace(
        '''            self.show_toast("Eine Ordneraktion läuft bereits.");''',
        '''            self.show_toast(&gettext("A folder operation is already running."));'''
    )

    content = content.replace(
        '''            self.show_toast("Der genaue Camel-Ordner ist nicht mehr verfügbar.");''',
        '''            self.show_toast(&gettext("The exact Camel folder is no longer available."));'''
    )

    # 21. Subscriptions & Properties dialogs
    content = content.replace(
        '''            self.show_toast(
                "Ordner-Abonnements sind nur für ein geladenes natives Mailkonto verfügbar",
            );''',
        '''            self.show_toast(
                &gettext("Folder subscriptions are only available for a loaded native mail account"),
            );'''
    )

    content = content.replace(
        '''            self.show_toast("Ordner-Eigenschaften sind nur für geladene, echte Ordner verfügbar");''',
        '''            self.show_toast(&gettext("Folder properties are only available for loaded, real folders"));'''
    )

    # 22. Empty trash & junk
    content = content.replace(
        '''            self.show_toast(
                "Dieser Kontoordner benötigt zum endgültigen Leeren den Linux-Expunge-Pfad; der lokale Bestand wurde nicht verändert.",
            );''',
        '''            self.show_toast(
                &gettext("This account folder requires the Linux expunge path for permanent emptying; local storage was not changed."),
            );'''
    )

    content = content.replace(
        '''                    message: "Papierkorb-Vorgang wurde abgebrochen".to_string(),''',
        '''                    message: gettext("Trash operation was cancelled"),'''
    )

    content = content.replace(
        '''                            state.set_label(&format!(
                                "{count} lokale Nachricht(en) aus dem Papierkorb entfernt"
                            ));
                        }
                        page.show_toast(&format!("{success_label}: {count} Nachricht(en)"));
                    } else {
                        page.show_toast("Papierkorb ist bereits leer");
                    }
                }
                Err(error) => {
                    page.show_toast(&format!("Papierkorb konnte nicht geleert werden: {error}"))
                }''',
        '''                            let status = ngettext(
                                "{count} local message removed from trash",
                                "{count} local messages removed from trash",
                                count as u32,
                            ).replace("{count}", &count.to_string());
                            state.set_label(&status);
                        }
                        let count_str = ngettext(
                            "{count} message",
                            "{count} messages",
                            count as u32,
                        ).replace("{count}", &count.to_string());
                        page.show_toast(&format!("{success_label}: {count_str}"));
                    } else {
                        page.show_toast(&gettext("Trash is already empty"));
                    }
                }
                Err(error) => {
                    page.show_toast(&gettext("Trash could not be emptied: {error}").replace("{error}", &error.to_string()))
                }'''
    )

    content = content.replace(
        '''                    message: "Spam-Vorgang wurde abgebrochen".to_string(),''',
        '''                    message: gettext("Junk operation was cancelled"),'''
    )

    content = content.replace(
        '''                            state.set_label(&format!(
                                "{count} lokale Nachricht(en) aus dem Spam-Ordner entfernt"
                            ));
                        }
                        page.show_toast(&format!("{success_label}: {count} Nachricht(en)"));
                    } else {
                        page.show_toast("Spam-Ordner ist bereits leer");
                    }
                }
                Err(error) => {
                    page.show_toast(&format!("Spam-Ordner konnte nicht geleert werden: {error}"))
                }''',
        '''                            let status = ngettext(
                                "{count} local message removed from junk folder",
                                "{count} local messages removed from junk folder",
                                count as u32,
                            ).replace("{count}", &count.to_string());
                            state.set_label(&status);
                        }
                        let count_str = ngettext(
                            "{count} message",
                            "{count} messages",
                            count as u32,
                        ).replace("{count}", &count.to_string());
                        page.show_toast(&format!("{success_label}: {count_str}"));
                    } else {
                        page.show_toast(&gettext("Junk folder is already empty"));
                    }
                }
                Err(error) => {
                    page.show_toast(&gettext("Junk folder could not be emptied: {error}").replace("{error}", &error.to_string()))
                }'''
    )

    # 23. Follow up editor
    content = content.replace(
        '''        let dialog = adw::Window::builder()
            .title("Nachverfolgung")
            .modal(true)
            .default_width(420)
            .build();
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label("Abbrechen");
        let save = gtk::Button::builder()
            .label("Speichern")
            .css_classes(vec!["suggested-action"])
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        let group = adw::PreferencesGroup::builder()
            .title("Nachverfolgung")
            .build();
        let text = adw::EntryRow::builder().title("Kennzeichnung").build();
        let due = adw::EntryRow::builder()
            .title("Fällig (YYYY-MM-DD HH:MM, optional)")
            .build();''',
        '''        let title = gettext("Follow-up");
        let dialog = adw::Window::builder()
            .title(&title)
            .modal(true)
            .default_width(420)
            .build();
        if let Some(root) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_transient_for(Some(&root));
        }

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label(&gettext("Cancel"));
        let save = gtk::Button::builder()
            .label(&gettext("Save"))
            .css_classes(vec!["suggested-action"])
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(20)
            .margin_end(20)
            .margin_top(16)
            .margin_bottom(20)
            .build();
        let group = adw::PreferencesGroup::builder()
            .title(&gettext("Follow-up"))
            .build();
        let text = adw::EntryRow::builder().title(&gettext("Flag")).build();
        let due = adw::EntryRow::builder()
            .title(&gettext("Due (YYYY-MM-DD HH:MM, optional)"))
            .build();'''
    )

    content = content.replace(
        '''            let warning = gtk::Label::builder()
                .label("Microsoft Graph unterstützt keine Follow-up-Fälligkeit.")''',
        '''            let warning = gtk::Label::builder()
                .label(&gettext("Microsoft Graph does not support follow-up due dates."))'''
    )

    content = content.replace(
        '''        let clear = gtk::Button::with_label("Entfernen");
        let complete = gtk::Button::with_label("Als erledigt markieren");''',
        '''        let clear = gtk::Button::with_label(&gettext("Clear"));
        let complete = gtk::Button::with_label(&gettext("Mark as Completed"));'''
    )

    content = content.replace(
        '''page.show_toast(&format!("Nachverfolgung nicht möglich: {error}"))''',
        '''page.show_toast(&gettext("Follow-up not possible: {error}").replace("{error}", &error.to_string()))'''
    )

    # 24. Label editor
    content = content.replace(
        '''            self.execute_mail_batch(commands, "Labels ändern")?;''',
        '''            self.execute_mail_batch(commands, &gettext("Change labels"))?;'''
    )

    content = content.replace(
        '''            self.show_toast("Keine Nachricht ausgewählt");''',
        '''            self.show_toast(&gettext("No message selected"));'''
    )

    content = content.replace(
        '''            .title(if has_batch {
                "Labels für Auswahl"
            } else {
                "Labels der Nachricht"
            })''',
        '''            .title(if has_batch {
                &gettext("Labels for Selection")
            } else {
                &gettext("Message Labels")
            })'''
    )

    content = content.replace(
        '''        let close_button = gtk::Button::builder()
            .label("Schließen")''',
        '''        let close_button = gtk::Button::builder()
            .label(&gettext("Close"))'''
    )

    content = content.replace(
        '''        let group_labels = adw::PreferencesGroup::builder()
            .title("Standard- und benutzerdefinierte Labels")
            .build();''',
        '''        let group_labels = adw::PreferencesGroup::builder()
            .title(&gettext("Standard and Custom Labels"))
            .build();'''
    )

    content = content.replace(
        '''                    page.set_batch_label(&tag_for_toggle, active)
                        .map(|count| format!("{count} Nachricht(en) aktualisiert"))
                } else {
                    page.set_selected_label(&tag_for_toggle, active).map(|_| {
                        if active {
                            "Label hinzugefügt".to_string()
                        } else {
                            "Label entfernt".to_string()
                        }
                    })
                };
                match result {
                    Ok(msg) => page.show_toast(&msg),
                    Err(error) => {
                        page.show_toast(&format!("Label konnte nicht aktualisiert werden: {error}"))
                    }
                }''',
        '''                    page.set_batch_label(&tag_for_toggle, active)
                        .map(|count| {
                            ngettext(
                                "{count} message updated",
                                "{count} messages updated",
                                count as u32,
                            ).replace("{count}", &count.to_string())
                        })
                } else {
                    page.set_selected_label(&tag_for_toggle, active).map(|_| {
                        if active {
                            gettext("Label added")
                        } else {
                            gettext("Label removed")
                        }
                    })
                };
                match result {
                    Ok(msg) => page.show_toast(&msg),
                    Err(error) => {
                        page.show_toast(&gettext("Label could not be updated: {error}").replace("{error}", &error.to_string()))
                    }
                }'''
    )

    content = content.replace(
        '''        let group_custom = adw::PreferencesGroup::builder()
            .title("Freies Label")
            .description("Eigenen Tag oder Labelnamen angeben")
            .build();

        let label_entry = adw::EntryRow::builder()
            .title("Freitext-Label")
            .show_apply_button(false)
            .build();
        group_custom.add(&label_entry);

        let custom_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(4)
            .build();

        let set_custom_btn = gtk::Button::builder()
            .label("Label setzen")
            .css_classes(vec!["suggested-action", "flat"])
            .build();
        let remove_custom_btn = gtk::Button::builder()
            .label("Label entfernen")
            .css_classes(vec!["destructive-action", "flat"])
            .build();''',
        '''        let group_custom = adw::PreferencesGroup::builder()
            .title(&gettext("Custom Label"))
            .description(&gettext("Specify custom tag or label name"))
            .build();

        let label_entry = adw::EntryRow::builder()
            .title(&gettext("Custom label"))
            .show_apply_button(false)
            .build();
        group_custom.add(&label_entry);

        let custom_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(4)
            .build();

        let set_custom_btn = gtk::Button::builder()
            .label(&gettext("Set Label"))
            .css_classes(vec!["suggested-action", "flat"])
            .build();
        let remove_custom_btn = gtk::Button::builder()
            .label(&gettext("Remove Label"))
            .css_classes(vec!["destructive-action", "flat"])
            .build();'''
    )

    content = content.replace(
        '''            if label.is_empty() {
                page.show_toast("Bitte einen Labelnamen eingeben");
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, true)
                    .map(|count| format!("{count} Nachricht(en) mit Label versehen"))
            } else {
                page.set_selected_label(&label, true)
                    .map(|_| "Label gesetzt".to_string())
            };
            match result {
                Ok(message) => page.show_toast(&message),
                Err(error) => {
                    page.show_toast(&format!("Label konnte nicht gesetzt werden: {error}"))
                }
            }''',
        '''            if label.is_empty() {
                page.show_toast(&gettext("Please enter a label name"));
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, true)
                    .map(|count| {
                        ngettext(
                            "{count} message labeled",
                            "{count} messages labeled",
                            count as u32,
                        ).replace("{count}", &count.to_string())
                    })
            } else {
                page.set_selected_label(&label, true)
                    .map(|_| gettext("Label set"))
            };
            match result {
                Ok(message) => page.show_toast(&message),
                Err(error) => {
                    page.show_toast(&gettext("Label could not be set: {error}").replace("{error}", &error.to_string()))
                }
            }'''
    )

    content = content.replace(
        '''            if label.is_empty() {
                page.show_toast("Bitte einen Labelnamen eingeben");
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, false)
                    .map(|count| format!("Label von {count} Nachricht(en) entfernt"))
            } else {
                page.set_selected_label(&label, false)
                    .map(|_| "Label entfernt".to_string())
            };
            match result {
                Ok(message) => page.show_toast(&message),
                Err(error) => {
                    page.show_toast(&format!("Label konnte nicht entfernt werden: {error}"))
                }
            }''',
        '''            if label.is_empty() {
                page.show_toast(&gettext("Please enter a label name"));
                return;
            }
            let result = if has_batch {
                page.set_batch_label(&label, false)
                    .map(|count| {
                        ngettext(
                            "Label removed from {count} message",
                            "Label removed from {count} messages",
                            count as u32,
                        ).replace("{count}", &count.to_string())
                    })
            } else {
                page.set_selected_label(&label, false)
                    .map(|_| gettext("Label removed"))
            };
            match result {
                Ok(message) => page.show_toast(&message),
                Err(error) => {
                    page.show_toast(&gettext("Label could not be removed: {error}").replace("{error}", &error.to_string()))
                }
            }'''
    )

    # 25. Destination dialog
    content = content.replace(
        '''            self.show_toast("Keine verfügbaren Zielordner");''',
        '''            self.show_toast(&gettext("No destination folders available"));'''
    )

    content = content.replace(
        '''        let title = if copy {
            "Nachricht kopieren"
        } else {
            "Nachricht verschieben"
        };''',
        '''        let title = if copy {
            gettext("Copy Message")
        } else {
            gettext("Move Message")
        };'''
    )

    content = content.replace(
        '''                        page.show_toast(if copy {
                            "Nachricht wird kopiert"
                        } else {
                            "Nachricht wird verschoben"
                        });''',
        '''                        page.show_toast(&if copy {
                            gettext("Copying message")
                        } else {
                            gettext("Moving message")
                        });'''
    )

    content = content.replace(
        '''                    Ok(false) => page.show_toast("Keine Nachricht ausgewählt"),
                    Err(error) => page.show_toast(&format!("{title} nicht möglich: {error}")),''',
        '''                    Ok(false) => page.show_toast(&gettext("No message selected")),
                    Err(error) => page.show_toast(&gettext("{title} not possible: {error}").replace("{title}", &title).replace("{error}", &error.to_string())),'''
    )

    # 26. Filter report
    content = content.replace(
        '''                    if report.matched_messages == 0 {
                        page.show_toast("Keine passenden Nachrichten für aktive Filterregeln");
                    } else {
                        page.show_toast(&format!(
                            "Filter angewendet: {} Treffer · {} verschoben · {} gelöscht",
                            report.matched_messages, report.moved_messages, report.deleted_messages,
                        ));
                    }''',
        '''                    if report.matched_messages == 0 {
                        page.show_toast(&gettext("No matching messages for active filter rules"));
                    } else {
                        page.show_toast(&gettext("Filters applied: {matched} matches · {moved} moved · {deleted} deleted")
                            .replace("{matched}", &report.matched_messages.to_string())
                            .replace("{moved}", &report.moved_messages.to_string())
                            .replace("{deleted}", &report.deleted_messages.to_string()));
                    }'''
    )

    # 27. move/copy backend errors
    content = content.replace('"Nachricht aus einem Konto in den lokalen Import"', '"message from account to local import"')
    content = content.replace('"lokaler Import in ein Konto"', '"local import to an account"')
    content = content.replace('"anderes Konto"', '"different account"')
    content = content.replace('"Online-Nachricht in den lokalen Import"', '"online message to local import"')
    content = content.replace('"lokale Kopie einer Kopie"', '"local copy of a copy"')
    content = content.replace('"lokaler Import in ein Graph-Konto"', '"local import to a Graph account"')
    content = content.replace('"anderes Online-Konto"', '"different online account"')
    content = content.replace('"Kopieren für dieses Konto (Linux-Abnahme ausstehend)"', '"copying for this account (pending Linux signoff)"')
    content = content.replace('"gemischte lokale/Konto-Auswahl"', '"mixed local/account selection"')
    content = content.replace('"kein gemeinsames Konto"', '"no common account"')
    content = content.replace('"gemischte lokale/Remote-Auswahl"', '"mixed local/remote selection"')

    # 28. Toast in move_message_by_id_to / copy_message_by_id_to
    content = content.replace(
        '''            self.show_toast(&format!("Nachricht nach „{}“ verschoben", target_display));''',
        '''            self.show_toast(&gettext("Message moved to “{target}”").replace("{target}", &target_display));'''
    )

    content = content.replace(
        '''            self.show_toast(&format!("Nachricht nach „{}“ kopiert", target_display));''',
        '''            self.show_toast(&gettext("Message copied to “{target}”").replace("{target}", &target_display));'''
    )

    # 29. Execution batch status / toasts
    content = content.replace(
        '''                            let status = if cancellation_requested {
                                "Vorgang wurde bereits serverseitig abgeschlossen".to_string()
                            } else {
                                format!("{description} abgeschlossen")
                            };
                            state.set_label(&status);''',
        '''                            let status = if cancellation_requested {
                                gettext("Operation has already completed on the server")
                            } else {
                                gettext("{description} completed").replace("{description}", description)
                            };
                            state.set_label(&status);'''
    )

    content = content.replace(
        '''                        page.report_operation_error(&description, &error);
                        page.show_toast(&format!("{description} nicht möglich: {error}"));''',
        '''                        page.report_operation_error(&description, &error);
                        page.show_toast(&gettext("{description} not possible: {error}").replace("{description}", description).replace("{error}", &error.to_string()));'''
    )

    content = content.replace(
        '''                        "Mail-Worker wurde unerwartet beendet",''',
        '''                        "Mail worker terminated unexpectedly",'''
    )

    content = content.replace(
        '''                            state.set_label(&format!("{description}: {applied} abgeschlossen"));''',
        '''                            state.set_label(&gettext("{description}: {count} completed").replace("{description}", description).replace("{count}", &applied.to_string()));'''
    )

    content = content.replace(
        '''                        "Mail-Batch-Worker wurde unerwartet beendet",''',
        '''                        "Mail batch worker terminated unexpectedly",'''
    )

    content = content.replace(
        '''                message: "anderer Mailaktion".to_string(),''',
        '''                message: "another mail action".to_string(),'''
    )

    content = content.replace(
        '''        let description = if undo { "Rückgängig" } else { "Wiederholen" };''',
        '''        let description = if undo { "Undo" } else { "Redo" };'''
    )

    content = content.replace(
        '''            state.set_label(&format!("{description} …"));''',
        '''            let localized = gettext(description);
            state.set_label(&format!("{localized} …"));'''
    )

    content = content.replace(
        '''                            state.set_label(&format!("{description} abgeschlossen"));''',
        '''                            state.set_label(&gettext("{description} completed").replace("{description}", &gettext(description)));'''
    )

    content = content.replace(
        '''                            state.set_label(&format!("Nichts zum {description}"));''',
        '''                            state.set_label(&gettext("Nothing to {description}").replace("{description}", &gettext(description).to_lowercase()));'''
    )

    content = content.replace(
        '''                        "Mail-Historien-Worker wurde unerwartet beendet",''',
        '''                        "Mail history worker terminated unexpectedly",'''
    )

    content = content.replace(
        '''            label.set_label(&format!("{count} ausgewählt"));''',
        '''            let count_str = ngettext("{count} selected", "{count} selected", count as u32).replace("{count}", &count.to_string());
            label.set_label(&count_str);'''
    )

    content = content.replace(
        '''        if count > 0 {
            self.execute_mail_batch(commands, "Markierung ändern")?;
        }''',
        '''        if count > 0 {
            self.execute_mail_batch(commands, &gettext("Change flag"))?;
        }'''
    )

    content = content.replace(
        '''                backend: "Wichtigkeitsmarkierung dieses Kontos (Linux-Abnahme ausstehend)",''',
        '''                backend: "importance flag for this account (pending Linux signoff)",'''
    )

    content = content.replace(
        '''            self.execute_mail_batch(commands, "Wichtigkeit ändern")?;''',
        '''            self.execute_mail_batch(commands, &gettext("Change importance"))?;'''
    )

    content = content.replace(
        '''        self.execute_mail_batch(commands, "Spam-Markierung entfernen")?;''',
        '''        self.execute_mail_batch(commands, &gettext("Remove junk status"))?;'''
    )

    content = content.replace(
        '''        self.execute_mail_batch(commands, "Nachrichten löschen")?;''',
        '''        self.execute_mail_batch(commands, &gettext("Delete messages"))?;'''
    )

    content = content.replace(
        '''            for (folder_name, label) in [
                ("inbox", "Lokaler Posteingang"),
                ("sent", "Lokal · Gesendet"),
                ("drafts", "Lokal · Entwürfe"),
                ("trash", "Lokaler Papierkorb"),
                ("archive", "Lokales Archiv"),
                ("junk", "Lokaler Spam"),
            ] {
                destinations.push(MoveDestination {
                    source_uid: "local".to_string(),
                    folder_name: folder_name.to_string(),
                    label: label.to_string(),
                });
            }''',
        '''            for (folder_name, label) in [
                ("inbox", gettext("Local Inbox")),
                ("sent", gettext("Local · Sent")),
                ("drafts", gettext("Local · Drafts")),
                ("trash", gettext("Local Trash")),
                ("archive", gettext("Local Archive")),
                ("junk", gettext("Local Junk")),
            ] {
                destinations.push(MoveDestination {
                    source_uid: "local".to_string(),
                    folder_name: folder_name.to_string(),
                    label,
                });
            }'''
    )

    content = content.replace('"Verschieben nicht möglich"', '&gettext("Move not possible")')
    content = content.replace('"Kopieren nicht möglich"', '&gettext("Copy not possible")')
    content = content.replace('"Nachrichten verschieben"', '&gettext("Move messages")')
    content = content.replace('"Nachrichten kopieren"', '&gettext("Copy messages")')
    content = content.replace('state.set_label("Keine neue Kopie erforderlich");', 'state.set_label(&gettext("No new copy needed"));')

    # 30. Sidebar and drawer labels
    content = content.replace(
        '''                    if source.enabled() {
                        "aktiv"
                    } else {
                        "Konto nicht verfügbar"
                    }''',
        '''                    if source.enabled() {
                        &gettext("active")
                    } else {
                        &gettext("Account not available")
                    }'''
    )

    content = content.replace(
        '''                        page.show_toast("In diesen Ordner können keine Nachrichten abgelegt werden");''',
        '''                        page.show_toast(&gettext("Messages cannot be placed in this folder"));'''
    )

    content = content.replace(
        '''                            page.show_toast(&format!("Aktion nicht möglich: {err}"));''',
        '''                            page.show_toast(&gettext("Action not possible: {error}").replace("{error}", &err.to_string()));'''
    )

    content = content.replace(
        '''        let local_folder_header = gtk::Label::builder()
            .label("Auf diesem Rechner")
            .css_classes(vec!["sidebar-section-header"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let new_local_folder_button = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("Lokalen Ordner erstellen")''',
        '''        let local_folder_header = gtk::Label::builder()
            .label(&gettext("On This Computer"))
            .css_classes(vec!["sidebar-section-header"])
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let new_local_folder_button = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text(&gettext("Create local folder"))'''
    )

    content = content.replace(
        '''        let source_state = gtk::Label::builder()
            .label("Konten werden geladen …")''',
        '''        let source_state = gtk::Label::builder()
            .label(&gettext("Loading accounts…"))'''
    )

    content = content.replace(
        '''            let edit_button = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Gespeicherten Suchordner bearbeiten")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Gespeicherten Suchordner löschen")
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();''',
        '''            let edit_button = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text(&gettext("Edit saved search folder"))
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();
            let delete_button = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(&gettext("Delete saved search folder"))
                .css_classes(vec!["flat", "circular"])
                .valign(gtk::Align::Center)
                .build();'''
    )

    content = content.replace(
        '''                crate::dialogs::run_destructive_action(
                    button,
                    "Gespeicherten Suchordner löschen?",
                    "Die lokale Suchdefinition wird dauerhaft entfernt.",
                    "Löschen",''',
        '''                let heading = gettext("Delete saved search folder?");
                let body = gettext("The local search definition will be permanently removed.");
                let del_lbl = gettext("Delete");
                crate::dialogs::run_destructive_action(
                    button,
                    &heading,
                    &body,
                    &del_lbl,'''
    )

    content = content.replace(
        '''        let rename_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Lokalen Ordner umbenennen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Leeren lokalen Ordner löschen")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();''',
        '''        let rename_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(&gettext("Rename local folder"))
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(&gettext("Delete empty local folder"))
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();'''
    )

    content = content.replace(
        '''            crate::dialogs::run_destructive_action(
                button,
                "Lokalen Ordner löschen?",
                "Nur leere app-eigene Ordner können gelöscht werden. Importierte Maildateien bleiben unverändert.",
                "Löschen",''',
        '''            let heading = gettext("Delete local folder?");
            let body = gettext("Only empty app-owned folders can be deleted. Imported mail files remain unchanged.");
            let del_lbl = gettext("Delete");
            crate::dialogs::run_destructive_action(
                button,
                &heading,
                &body,
                &del_lbl,'''
    )

    # 31. format_drawer_folder_tooltip
    old_drawer_tooltip = '''        if unread > 0 {
            let total_str = if total == 1 {
                "1 gesamt".to_string()
            } else {
                format!("{total} gesamt")
            };
            if unread == 1 {
                format!("{title_display}: 1 ungelesen, {total_str}")
            } else {
                format!("{title_display}: {unread} ungelesen, {total_str}")
            }
        } else if is_sent {
            if total == 1 {
                format!("{title_display}: 1 gesendete E-Mail")
            } else {
                format!("{title_display}: {total} gesendete E-Mails")
            }
        } else if is_drafts {
            if total == 1 {
                format!("{title_display}: 1 Entwurf")
            } else {
                format!("{title_display}: {total} Entwürfe")
            }
        } else if is_trash {
            if total == 1 {
                format!("{title_display}: 1 gelöschte E-Mail")
            } else {
                format!("{title_display}: {total} gelöschte E-Mails")
            }
        } else if is_junk {
            if total == 1 {
                format!("{title_display}: 1 unerwünschte E-Mail")
            } else {
                format!("{title_display}: {total} unerwünschte E-Mails")
            }
        } else if total == 1 {
            format!("{title_display}: 1 E-Mail")
        } else {
            format!("{title_display}: {total} E-Mails")
        }'''

    new_drawer_tooltip = '''        if unread > 0 {
            let total_str = ngettext("1 total", "{} total", total as u32).replace("{}", &total.to_string());
            let unread_str = ngettext("1 unread", "{} unread", unread).replace("{}", &unread.to_string());
            format!("{title_display}: {unread_str}, {total_str}")
        } else if is_sent {
            let sent_str = ngettext("1 sent email", "{} sent emails", total as u32).replace("{}", &total.to_string());
            format!("{title_display}: {sent_str}")
        } else if is_drafts {
            let draft_str = ngettext("1 draft", "{} drafts", total as u32).replace("{}", &total.to_string());
            format!("{title_display}: {draft_str}")
        } else if is_trash {
            let trash_str = ngettext("1 deleted email", "{} deleted emails", total as u32).replace("{}", &total.to_string());
            format!("{title_display}: {trash_str}")
        } else if is_junk {
            let junk_str = ngettext("1 junk email", "{} junk emails", total as u32).replace("{}", &total.to_string());
            format!("{title_display}: {junk_str}")
        } else if total == 1 {
            format!("{title_display}: {}", gettext("1 email"))
        } else {
            let emails_str = ngettext("{} email", "{} emails", total as u32).replace("{}", &total.to_string());
            format!("{title_display}: {emails_str}")
        }'''

    content = content.replace(old_drawer_tooltip, new_drawer_tooltip)

    # 32. Toolbar actions in create_toolbars
    content = content.replace(
        '''        let prev_btn = make_button(
            "go-previous-symbolic",
            "Vorherige Nachricht (k)",
            "win.mail-prev",
        );
        let next_btn = make_button(
            "go-next-symbolic",
            "Nächste Nachricht (j)",
            "win.mail-next",
        );''',
        '''        let str_prev = gettext("Previous message (k)");
        let prev_btn = make_button(
            "go-previous-symbolic",
            &str_prev,
            "win.mail-prev",
        );
        let str_next = gettext("Next message (j)");
        let next_btn = make_button(
            "go-next-symbolic",
            &str_next,
            "win.mail-next",
        );'''
    )

    content = content.replace(
        '''        let read_section = gio::Menu::new();
        read_section.append(Some("Als gelesen markieren"), Some("win.mark-read"));
        read_section.append(Some("Als ungelesen markieren"), Some("win.mark-unread"));
        more_menu.append_section(None, &read_section);

        let flag_section = gio::Menu::new();
        flag_section.append(Some("Markierung umschalten (Stern)"), Some("win.toggle-flag"));
        flag_section.append(Some("Wichtigkeitsmarkierung umschalten"), Some("win.toggle-important"));
        more_menu.append_section(None, &flag_section);

        let junk_section = gio::Menu::new();
        junk_section.append(Some("Als Spam markieren"), Some("win.mark-junk"));
        junk_section.append(Some("Als Nicht-Spam markieren"), Some("win.mark-not-junk"));
        more_menu.append_section(None, &junk_section);

        let source_section = gio::Menu::new();
        source_section.append(Some("Nachrichtenquelle anzeigen"), Some("win.view-message-source"));
        more_menu.append_section(None, &source_section);''',
        '''        let read_section = gio::Menu::new();
        let str_mark_read = gettext("Mark as Read");
        let str_mark_unread = gettext("Mark as Unread");
        read_section.append(Some(&str_mark_read), Some("win.mark-read"));
        read_section.append(Some(&str_mark_unread), Some("win.mark-unread"));
        more_menu.append_section(None, &read_section);

        let flag_section = gio::Menu::new();
        let str_toggle_flag = gettext("Toggle Flag (Star)");
        let str_toggle_important = gettext("Toggle Importance");
        flag_section.append(Some(&str_toggle_flag), Some("win.toggle-flag"));
        flag_section.append(Some(&str_toggle_important), Some("win.toggle-important"));
        more_menu.append_section(None, &flag_section);

        let junk_section = gio::Menu::new();
        let str_mark_junk = gettext("Mark as Junk");
        let str_mark_not_junk = gettext("Mark as Not Junk");
        junk_section.append(Some(&str_mark_junk), Some("win.mark-junk"));
        junk_section.append(Some(&str_mark_not_junk), Some("win.mark-not-junk"));
        more_menu.append_section(None, &junk_section);

        let source_section = gio::Menu::new();
        let str_view_src = gettext("View Message Source");
        source_section.append(Some(&str_view_src), Some("win.view-message-source"));
        more_menu.append_section(None, &source_section);'''
    )

    content = content.replace(
        '''        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Seitenleiste ein-/ausblenden (F9)")
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, "Seitenleiste ein- oder ausblenden");''',
        '''        let str_sidebar_tip = gettext("Toggle sidebar (F9)");
        let str_sidebar_acc = gettext("Show or hide sidebar");
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(&str_sidebar_tip)
            .active(sidebar_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&sidebar_toggle, &str_sidebar_acc);'''
    )

    content = content.replace(
        '''        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Aufgabenleiste für heute ein-/ausblenden")
            .active(todo_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&preview_toggle, "Aufgabenleiste für heute ein- oder ausblenden");''',
        '''        let str_todo_tip = gettext("Toggle To Do bar");
        let str_todo_acc = gettext("Show or hide To Do bar");
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text(&str_todo_tip)
            .active(todo_visible)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&preview_toggle, &str_todo_acc);'''
    )

    # 33. Column chooser dialog
    content = content.replace(
        '''        let desc_label = gtk::Label::builder()
            .label("Passen Sie die Reihenfolge der Spalten an. Änderungen werden sofort wirksam.")''',
        '''        let desc_label = gtk::Label::builder()
            .label(&gettext("Adjust the order of columns. Changes take effect immediately."))'''
    )

    content = content.replace(
        '''                let label_text = match column.as_str() {
                    "status" => "Status",
                    "attachment" => "Anhang",
                    "flagged" => "Markierung",
                    "sender" => "Absender",
                    "recipient" => "Empfänger",
                    "subject" => "Betreff",
                    "date" => "Datum",
                    "follow-up" => "Nachverfolgung",
                    "important" => "Wichtigkeit",
                    "avatar" => "Avatar",
                    "preview" => "Vorschau",
                    "size" => "Größe",
                    "follow-up-due" => "Fällig am",
                    "location" => "Ort",
                    "messages" => "Nachrichten",
                    _ => column.as_str(),
                };
                let row = adw::ActionRow::builder()
                    .title(label_text)
                    .subtitle(column)
                    .build();
                let up = gtk::Button::builder()
                    .icon_name("go-up-symbolic")
                    .tooltip_text("Spalte nach oben / links verschieben")
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index > 0)
                    .build();
                let down = gtk::Button::builder()
                    .icon_name("go-down-symbolic")
                    .tooltip_text("Spalte nach unten / rechts verschieben")
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index + 1 < columns.len())
                    .build();''',
        '''                let label_text = match column.as_str() {
                    "status" => gettext("Status"),
                    "attachment" => gettext("Attachment"),
                    "flagged" => gettext("Flag"),
                    "sender" => gettext("Sender"),
                    "recipient" => gettext("Recipient"),
                    "subject" => gettext("Subject"),
                    "date" => gettext("Date"),
                    "follow-up" => gettext("Follow-up"),
                    "important" => gettext("Importance"),
                    "avatar" => gettext("Avatar"),
                    "preview" => gettext("Preview"),
                    "size" => gettext("Size"),
                    "follow-up-due" => gettext("Due by"),
                    "location" => gettext("Location"),
                    "messages" => gettext("Messages"),
                    _ => column.clone(),
                };
                let row = adw::ActionRow::builder()
                    .title(&label_text)
                    .subtitle(column)
                    .build();
                let up = gtk::Button::builder()
                    .icon_name("go-up-symbolic")
                    .tooltip_text(&gettext("Move column up / left"))
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index > 0)
                    .build();
                let down = gtk::Button::builder()
                    .icon_name("go-down-symbolic")
                    .tooltip_text(&gettext("Move column down / right"))
                    .css_classes(vec!["flat", "circular"])
                    .sensitive(index + 1 < columns.len())
                    .build();'''
    )

    content = content.replace(
        '''        let reset_btn = gtk::Button::builder()
            .label("Standard-Reihenfolge wiederherstellen")''',
        '''        let reset_btn = gtk::Button::builder()
            .label(&gettext("Restore Default Order"))'''
    )

    content = content.replace(
        '''        let title = gtk::Label::builder()
            .label("Spalten")''',
        '''        let title = gtk::Label::builder()
            .label(&gettext("Columns"))'''
    )

    content = content.replace(
        '''        let column_toggles: [(&'static str, &'static str); 10] = [
            ("mail-show-sender", "Absender"),
            ("mail-show-recipient", "Empfänger"),
            ("mail-show-date", "Datum"),
            ("mail-show-attachment", "Anhänge"),
            ("mail-show-preview-text", "Vorschautext"),
            ("mail-show-avatar", "Avatar"),
            ("mail-show-labels", "Schlagwörter"),
            ("mail-show-size", "Größe"),
            ("mail-show-follow-up-due", "Fällig am"),
            ("mail-show-location", "Ort"),
        ];''',
        '''        let column_toggles: [(&'static str, String); 10] = [
            ("mail-show-sender", gettext("Sender")),
            ("mail-show-recipient", gettext("Recipient")),
            ("mail-show-date", gettext("Date")),
            ("mail-show-attachment", gettext("Attachments")),
            ("mail-show-preview-text", gettext("Preview text")),
            ("mail-show-avatar", gettext("Avatar")),
            ("mail-show-labels", gettext("Labels")),
            ("mail-show-size", gettext("Size")),
            ("mail-show-follow-up-due", gettext("Due by")),
            ("mail-show-location", gettext("Location")),
        ];'''
    )

    content = content.replace(
        '''        let sort_reverse_btn = make_popover_action_button("view-sort-descending-symbolic", "Sortierrichtung umkehren");''',
        '''        let sort_reverse_btn = make_popover_action_button("view-sort-descending-symbolic", &gettext("Reverse sort order"));'''
    )

    content = content.replace(
        '''        let reorder_btn = make_popover_action_button(&reorder_icon_name, "Spaltenreihenfolge anpassen …");''',
        '''        let reorder_btn = make_popover_action_button(&reorder_icon_name, &gettext("Customize column order…"));'''
    )

    content = content.replace(
        '''        let reset_btn = make_popover_action_button("edit-undo-symbolic", "Standardspalten wiederherstellen");''',
        '''        let reset_btn = make_popover_action_button("edit-undo-symbolic", &gettext("Restore default columns"));'''
    )

    # 34. Header sort label updates
    content = content.replace(
        '''sender_label.set_label(&self.mail_header_sort_label("Von", MailSortField::Sender));''',
        '''sender_label.set_label(&self.mail_header_sort_label(&gettext("From"), MailSortField::Sender));'''
    )
    content = content.replace(
        '''recipient_label.set_label(&self.mail_header_sort_label("An", MailSortField::Recipient));''',
        '''recipient_label.set_label(&self.mail_header_sort_label(&gettext("To"), MailSortField::Recipient));'''
    )
    content = content.replace(
        '''lbl.set_label(&self.mail_header_sort_label("Betreff", MailSortField::Subject));''',
        '''lbl.set_label(&self.mail_header_sort_label(&gettext("Subject"), MailSortField::Subject));'''
    )
    content = content.replace(
        '''lbl.set_label("Nachrichten");''',
        '''lbl.set_label(&gettext("Messages"));'''
    )
    content = content.replace(
        '''lbl.set_label(&self.mail_header_sort_label("Datum", MailSortField::Date));''',
        '''lbl.set_label(&self.mail_header_sort_label(&gettext("Date"), MailSortField::Date));'''
    )
    content = content.replace(
        '''lbl.set_label(&self.mail_header_sort_label("Größe", MailSortField::Size));''',
        '''lbl.set_label(&self.mail_header_sort_label(&gettext("Size"), MailSortField::Size));'''
    )
    content = content.replace(
        '''lbl.set_label(
                    &self.mail_header_sort_label("Fällig am", MailSortField::FollowUpDue),
                );''',
        '''lbl.set_label(
                    &self.mail_header_sort_label(&gettext("Due by"), MailSortField::FollowUpDue),
                );'''
    )
    content = content.replace(
        '''lbl.set_label(&self.mail_header_sort_label("Ort", MailSortField::Location));''',
        '''lbl.set_label(&self.mail_header_sort_label(&gettext("Location"), MailSortField::Location));'''
    )
    content = content.replace(
        '''        let subject_label = gtk::Label::builder()
            .label("Betreff")''',
        '''        let subject_label = gtk::Label::builder()
            .label(&gettext("Subject"))'''
    )
    content = content.replace(
        '''        messages_column.append(
            &gtk::Label::builder()
                .label("Nachrichten")''',
        '''        messages_column.append(
            &gtk::Label::builder()
                .label(&gettext("Messages"))'''
    )

    # 35. Column tooltips
    content = content.replace(
        '''sender_column.set_tooltip_text(Some("Nach Absender sortieren (Ziehen zum Verschieben)"));''',
        '''sender_column.set_tooltip_text(Some(&gettext("Sort by sender (drag to reorder)")));'''
    )
    content = content.replace(
        '''recipient_column.set_tooltip_text(Some("Nach Empfänger sortieren (Ziehen zum Verschieben)"));''',
        '''recipient_column.set_tooltip_text(Some(&gettext("Sort by recipient (drag to reorder)")));'''
    )
    content = content.replace(
        '''status_column.set_tooltip_text(Some("Nach Lesestatus sortieren (Ziehen zum Verschieben)"));''',
        '''status_column.set_tooltip_text(Some(&gettext("Sort by read status (drag to reorder)")));'''
    )
    content = content.replace(
        '''date_column.set_tooltip_text(Some("Nach Datum sortieren (Ziehen zum Verschieben)"));''',
        '''date_column.set_tooltip_text(Some(&gettext("Sort by date (drag to reorder)")));'''
    )
    content = content.replace(
        '''attachment_column.set_tooltip_text(Some("Nach Anhängen sortieren (Ziehen zum Verschieben)"));''',
        '''attachment_column.set_tooltip_text(Some(&gettext("Sort by attachments (drag to reorder)")));'''
    )
    content = content.replace(
        '''star_column.set_tooltip_text(Some("Nach Markierung sortieren (Ziehen zum Verschieben)"));''',
        '''star_column.set_tooltip_text(Some(&gettext("Sort by flag (drag to reorder)")));'''
    )
    content = content.replace(
        '''follow_up_column.set_tooltip_text(Some("Nach Nachverfolgung sortieren (Ziehen zum Verschieben)"));''',
        '''follow_up_column.set_tooltip_text(Some(&gettext("Sort by follow-up (drag to reorder)")));'''
    )
    content = content.replace(
        '''important_column.set_tooltip_text(Some("Nach Wichtigkeit sortieren (Ziehen zum Verschieben)"));''',
        '''important_column.set_tooltip_text(Some(&gettext("Sort by importance (drag to reorder)")));'''
    )
    content = content.replace(
        '''subject_column.set_tooltip_text(Some("Nach Betreff sortieren (Ziehen zum Verschieben)"));''',
        '''subject_column.set_tooltip_text(Some(&gettext("Sort by subject (drag to reorder)")));'''
    )

    content = content.replace(
        '''        for (col_id, column, field, tooltip) in [
            (
                "size",
                &size_column,
                MailSortField::Size,
                "Nach Größe sortieren (Ziehen zum Verschieben)",
            ),
            (
                "follow-up-due",
                &due_column,
                MailSortField::FollowUpDue,
                "Nach Fälligkeit sortieren (Ziehen zum Verschieben)",
            ),
            (
                "location",
                &location_column,
                MailSortField::Location,
                "Nach Ort sortieren (Ziehen zum Verschieben)",
            ),
        ] {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            self.setup_column_header_drag_and_sort(col_id, column, Some(field), &gesture, &header);
            column.set_tooltip_text(Some(tooltip));
            column.add_controller(gesture);
        }''',
        '''        for (col_id, column, field, tooltip) in [
            (
                "size",
                &size_column,
                MailSortField::Size,
                gettext("Sort by size (drag to reorder)"),
            ),
            (
                "follow-up-due",
                &due_column,
                MailSortField::FollowUpDue,
                gettext("Sort by due date (drag to reorder)"),
            ),
            (
                "location",
                &location_column,
                MailSortField::Location,
                gettext("Sort by location (drag to reorder)"),
            ),
        ] {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            self.setup_column_header_drag_and_sort(col_id, column, Some(field), &gesture, &header);
            column.set_tooltip_text(Some(&tooltip));
            column.add_controller(gesture);
        }'''
    )

    content = content.replace(
        '''        for (col_id, column, tooltip) in [
            ("avatar", &avatar_spacer, "Avatar-Spalte (Ziehen zum Verschieben)"),
            ("preview", &preview_column, "Vorschau-Spalte (Ziehen zum Verschieben)"),
            ("messages", &messages_column, "Nachrichten-Spalte (Ziehen zum Verschieben)"),
        ] {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            self.setup_column_header_drag_and_sort(col_id, column, None, &gesture, &header);
            column.set_tooltip_text(Some(tooltip));
            column.add_controller(gesture);
        }''',
        '''        for (col_id, column, tooltip) in [
            ("avatar", &avatar_spacer, gettext("Avatar column (drag to reorder)")),
            ("preview", &preview_column, gettext("Preview column (drag to reorder)")),
            ("messages", &messages_column, gettext("Messages column (drag to reorder)")),
        ] {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            self.setup_column_header_drag_and_sort(col_id, column, None, &gesture, &header);
            column.set_tooltip_text(Some(&tooltip));
            column.add_controller(gesture);
        }'''
    )

    # 36. Row action buttons & accessible labels
    content = content.replace(
        '''.tooltip_text("Markieren / Sternchen setzen")''',
        '''.tooltip_text(&gettext("Star / mark message"))'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_star, "Nachricht markieren");''',
        '''set_accessible_label(&btn_star, &gettext("Star message"));'''
    )
    content = content.replace(
        '''.tooltip_text("Nachverfolgung setzen (Rechtsklick für Optionen)")''',
        '''.tooltip_text(&gettext("Set follow-up (right-click for options)"))'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_follow_up, "Nachverfolgung setzen");''',
        '''set_accessible_label(&btn_follow_up, &gettext("Set follow-up"));'''
    )
    content = content.replace(
        '''.tooltip_text("Als wichtig markieren")''',
        '''.tooltip_text(&gettext("Mark as important"))'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_important, "Nachricht als wichtig markieren");''',
        '''set_accessible_label(&btn_important, &gettext("Mark message as important"));'''
    )
    content = content.replace(
        '''btn_star_clone.set_tooltip_text(Some("Markierung entfernen"));''',
        '''btn_star_clone.set_tooltip_text(Some(&gettext("Remove flag")));'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_star_clone, "Markierung entfernen");''',
        '''set_accessible_label(&btn_star_clone, &gettext("Remove flag"));'''
    )
    content = content.replace(
        '''btn_star_clone.set_tooltip_text(Some("Markieren / Sternchen setzen"));''',
        '''btn_star_clone.set_tooltip_text(Some(&gettext("Star / mark message")));'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_star_clone, "Nachricht markieren");''',
        '''set_accessible_label(&btn_star_clone, &gettext("Star message"));'''
    )
    content = content.replace(
        '''message: "Mailansicht ist nicht mehr verfügbar".to_string(),''',
        '''message: gettext("Mail view is no longer available"),'''
    )
    content = content.replace(
        '''btn_follow_up_clone.set_tooltip_text(Some("Nachverfolgung setzen (Rechtsklick für Optionen)"));''',
        '''btn_follow_up_clone.set_tooltip_text(Some(&gettext("Set follow-up (right-click for options)")));'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_follow_up_clone, "Nachverfolgung setzen");''',
        '''set_accessible_label(&btn_follow_up_clone, &gettext("Set follow-up"));'''
    )
    content = content.replace(
        '''btn_follow_up_clone.set_tooltip_text(Some("Als erledigt markieren (Rechtsklick für Optionen)"));''',
        '''btn_follow_up_clone.set_tooltip_text(Some(&gettext("Mark as completed (right-click for options)")));'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_follow_up_clone, "Nachverfolgung als erledigt markieren");''',
        '''set_accessible_label(&btn_follow_up_clone, &gettext("Mark follow-up as completed"));'''
    )
    content = content.replace(
        '''btn_follow_up_clone.set_tooltip_text(Some("Nachverfolgung entfernen (Rechtsklick für Optionen)"));''',
        '''btn_follow_up_clone.set_tooltip_text(Some(&gettext("Remove follow-up (right-click for options)")));'''
    )
    content = content.replace(
        '''set_accessible_label(&btn_follow_up_clone, "Nachverfolgung entfernen");''',
        '''set_accessible_label(&btn_follow_up_clone, &gettext("Remove follow-up"));'''
    )
    content = content.replace(
        '''page.show_toast(&format!("Wichtig-Markierung nicht möglich: {error}"));''',
        '''page.show_toast(&gettext("Importance flag not possible: {error}").replace("{error}", &error.to_string()));'''
    )
    content = content.replace(
        '''attachment.set_tooltip_text(Some("Anhänge"));''',
        '''attachment.set_tooltip_text(Some(&gettext("Attachments")));'''
    )
    content = content.replace(
        '''set_accessible_label(&thread_expander, "Konversation auf- oder zuklappen");''',
        '''set_accessible_label(&thread_expander, &gettext("Expand or collapse conversation"));'''
    )

    # 37. List item accessible label / description
    content = content.replace(
        '''            let thread_label = if visible_thread_size > 1 {
                format!("{} Nachrichten", visible_thread_size)
            } else {
                "Einzelne Nachricht".to_string()
            };
            list_item.set_accessible_label(&format!(
                "{} von {} · {} · {}",
                msg.subject(),
                sender_header(&msg),
                format_message_date(&msg.date_str()),
                thread_label
            ));
            list_item.set_accessible_description(&format!(
                "{}{}{}",
                if msg.is_unread() {
                    "Ungelesen. "
                } else {
                    "Gelesen. "
                },
                if msg.is_flagged() { "Markiert. " } else { "" },
                if msg.attachments().is_empty() {
                    "Keine Anhänge."
                } else {
                    "Anhänge vorhanden."
                }
            ));''',
        '''            let thread_label = if visible_thread_size > 1 {
                ngettext("{} message", "{} messages", visible_thread_size as u32).replace("{}", &visible_thread_size.to_string())
            } else {
                gettext("Single message")
            };
            list_item.set_accessible_label(&gettext("“{subject}” from {sender} · {date} · {thread}")
                .replace("{subject}", &msg.subject())
                .replace("{sender}", &sender_header(&msg))
                .replace("{date}", &format_message_date(&msg.date_str()))
                .replace("{thread}", &thread_label));
            list_item.set_accessible_description(&format!(
                "{}{}{}",
                if msg.is_unread() {
                    gettext("Unread. ")
                } else {
                    gettext("Read. ")
                },
                if msg.is_flagged() { gettext("Flagged. ") } else { String::new() },
                if msg.attachments().is_empty() {
                    gettext("No attachments.")
                } else {
                    gettext("Attachments present.")
                }
            ));'''
    )

    # 38. Header box follow up / star / important tooltips
    content = content.replace(
        '''                        button.set_tooltip_text(Some(
                            "Nachverfolgung setzen (Rechtsklick für Optionen)",
                        ));
                        set_accessible_label(&button, "Nachverfolgung setzen");''',
        '''                        let str_fu = gettext("Set follow-up (right-click for options)");
                        button.set_tooltip_text(Some(&str_fu));
                        set_accessible_label(&button, &gettext("Set follow-up"));'''
    )
    content = content.replace(
        '''                        button.set_tooltip_text(Some(
                            "Als erledigt markieren (Rechtsklick für Optionen)",
                        ));
                        set_accessible_label(&button, "Nachverfolgung als erledigt markieren");''',
        '''                        let str_fu_done = gettext("Mark as completed (right-click for options)");
                        button.set_tooltip_text(Some(&str_fu_done));
                        set_accessible_label(&button, &gettext("Mark follow-up as completed"));'''
    )
    content = content.replace(
        '''                        button.set_tooltip_text(Some(
                            "Nachverfolgung entfernen (Rechtsklick für Optionen)",
                        ));
                        set_accessible_label(&button, "Nachverfolgung entfernen");''',
        '''                        let str_fu_rm = gettext("Remove follow-up (right-click for options)");
                        button.set_tooltip_text(Some(&str_fu_rm));
                        set_accessible_label(&button, &gettext("Remove follow-up"));'''
    )
    content = content.replace(
        '''                    btn_star.set_tooltip_text(Some(
                        "Dieses Konto verwendet die separate Nachverfolgungsspalte",
                    ));''',
        '''                    let str_star_fu = gettext("This account uses the separate follow-up column");
                    btn_star.set_tooltip_text(Some(&str_star_fu));'''
    )
    content = content.replace(
        '''                    btn_star.set_tooltip_text(Some("Markieren / Sternchen setzen"));''',
        '''                    let str_star_tip = gettext("Star / mark message");
                    btn_star.set_tooltip_text(Some(&str_star_tip));'''
    )
    content = content.replace(
        '''                    set_accessible_label(&btn_star, "Markierung entfernen");''',
        '''                    set_accessible_label(&btn_star, &gettext("Remove flag"));'''
    )
    content = content.replace(
        '''                    set_accessible_label(
                        &btn_star,
                        "Sternmarkierung nicht verfügbar; Nachverfolgungsspalte verwenden",
                    );''',
        '''                    set_accessible_label(
                        &btn_star,
                        &gettext("Star not available; use follow-up column"),
                    );'''
    )
    content = content.replace(
        '''                set_accessible_label(
                    &btn_important,
                    "Nachricht als wichtig markieren",
                );''',
        '''                set_accessible_label(
                    &btn_important,
                    &gettext("Mark message as important"),
                );'''
    )

    # 39. View profiles & options popover
    content = content.replace(
        '''        let messages_profile_button = gtk::ToggleButton::builder()
            .label("Standard")
            .tooltip_text("Kompakte, mehrzeilige E-Mail-Liste")
            .build();
        let wide_profile_button = gtk::ToggleButton::builder()
            .label("Breitbild")
            .tooltip_text("Tabellarische Liste mit Spalten")
            .build();
        let cards_profile_button = gtk::ToggleButton::builder()
            .label("Karten")
            .tooltip_text("Großzügige Kartenansicht (Gmail / Outlook Stil)")
            .build();''',
        '''        let messages_profile_button = gtk::ToggleButton::builder()
            .label(&gettext("Standard"))
            .tooltip_text(&gettext("Compact, multi-line email list"))
            .build();
        let wide_profile_button = gtk::ToggleButton::builder()
            .label(&gettext("Wide"))
            .tooltip_text(&gettext("Tabular list with columns"))
            .build();
        let cards_profile_button = gtk::ToggleButton::builder()
            .label(&gettext("Cards"))
            .tooltip_text(&gettext("Spacious card view (Gmail / Outlook style)"))
            .build();'''
    )

    content = content.replace(
        '''        columns_popover_box.append(
            &gtk::Label::builder()
                .label("Kopfzeilen-Stil")
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading", "view-options-heading"])
                .build(),
        );
        let header_style_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .homogeneous(true)
            .css_classes(vec!["linked"])
            .build();
        let modern_header_button = gtk::ToggleButton::builder()
            .label("Modern")
            .tooltip_text("Webmail-Stil mit Avatar, prominentem Absender und Klapp-Details")
            .build();
        let classic_header_button = gtk::ToggleButton::builder()
            .label("Klassisch")
            .tooltip_text("Traditionelle tabellarische Kopfzeilen (GNOME Evolution)")
            .build();''',
        '''        columns_popover_box.append(
            &gtk::Label::builder()
                .label(&gettext("Header Style"))
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading", "view-options-heading"])
                .build(),
        );
        let header_style_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .homogeneous(true)
            .css_classes(vec!["linked"])
            .build();
        let modern_header_button = gtk::ToggleButton::builder()
            .label(&gettext("Modern"))
            .tooltip_text(&gettext("Webmail style with avatar, prominent sender, and collapsible details"))
            .build();
        let classic_header_button = gtk::ToggleButton::builder()
            .label(&gettext("Classic"))
            .tooltip_text(&gettext("Traditional tabular headers (GNOME Evolution)"))
            .build();'''
    )

    content = content.replace(
        '''        columns_popover_box.append(
            &gtk::Label::builder()
                .label("Gruppierung & Anzeige")
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading", "view-options-heading"])
                .build(),
        );

        let column_settings = [
            ("mail-show-avatar", "Avatare"),
            ("mail-show-sender", "Absender"),
            ("mail-show-recipient", "Empfänger"),
            ("mail-show-date", "Datum"),
            ("mail-show-attachment", "Anhänge"),
            ("mail-show-preview-text", "Vorschautext"),
            ("mail-show-size", "Größe"),
            ("mail-show-follow-up-due", "Fällig am"),
        ];''',
        '''        columns_popover_box.append(
            &gtk::Label::builder()
                .label(&gettext("Grouping & Display"))
                .halign(gtk::Align::Start)
                .css_classes(vec!["heading", "view-options-heading"])
                .build(),
        );

        let column_settings: [(&'static str, String); 8] = [
            ("mail-show-avatar", gettext("Avatars")),
            ("mail-show-sender", gettext("Sender")),
            ("mail-show-recipient", gettext("Recipient")),
            ("mail-show-date", gettext("Date")),
            ("mail-show-attachment", gettext("Attachments")),
            ("mail-show-preview-text", gettext("Preview text")),
            ("mail-show-size", gettext("Size")),
            ("mail-show-follow-up-due", gettext("Due by")),
        ];'''
    )

    content = content.replace(
        '''            let check = gtk::CheckButton::builder()
                .label(label)''',
        '''            let check = gtk::CheckButton::builder()
                .label(&label)'''
    )

    content = content.replace(
        '''            let threading_check = gtk::CheckButton::builder()
                .label("Nach Unterhaltungen gruppieren")
                .tooltip_text("Zusammengehörige E-Mails in Diskussionsfäden bündeln")''',
        '''            let threading_check = gtk::CheckButton::builder()
                .label(&gettext("Group by conversations"))
                .tooltip_text(&gettext("Group related emails into discussion threads"))'''
    )

    content = content.replace(
        '''            let show_deleted_check = gtk::CheckButton::builder()
                .label("Gelöschte Nachrichten anzeigen")
                .tooltip_text("Als gelöscht markierte E-Mails durchgestrichen in der Liste belassen")''',
        '''            let show_deleted_check = gtk::CheckButton::builder()
                .label(&gettext("Show deleted messages"))
                .tooltip_text(&gettext("Leave emails marked for deletion struck through in the list"))'''
    )

    content = content.replace(
        '''                    .label("Einstellungen sind in diesem Entwicklungsmodus nicht verfügbar.")''',
        '''                    .label(&gettext("Settings are not available in this development mode."))'''
    )

    content = content.replace(
        '''        let columns_button = gtk::MenuButton::builder()
            .icon_name("view-column-symbolic")
            .tooltip_text("Ansichtsoptionen")
            .popover(&columns_popover)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&columns_button, "Ansichtsoptionen");''',
        '''        let str_view_opts = gettext("View options");
        let columns_button = gtk::MenuButton::builder()
            .icon_name("view-column-symbolic")
            .tooltip_text(&str_view_opts)
            .popover(&columns_popover)
            .css_classes(vec!["flat"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&columns_button, &str_view_opts);'''
    )

    content = content.replace(
        '''        let search_options_header = gtk::Label::builder()
            .label("Suchoptionen & Filter")''',
        '''        let search_options_header = gtk::Label::builder()
            .label(&gettext("Search Options & Filters"))'''
    )

    content = content.replace(
        '''        let lbl_field = gtk::Label::builder()
            .label("Suchfeld:")''',
        '''        let lbl_field = gtk::Label::builder()
            .label(&gettext("Search field:"))'''
    )

    content = content.replace(
        '''        let lbl_scope = gtk::Label::builder()
            .label("Suchbereich:")''',
        '''        let lbl_scope = gtk::Label::builder()
            .label(&gettext("Search scope:"))'''
    )

    content = content.replace(
        '''        let lbl_filter = gtk::Label::builder()
            .label("Filter:")''',
        '''        let lbl_filter = gtk::Label::builder()
            .label(&gettext("Filter:"))'''
    )

    content = content.replace(
        '''        let syntax_header = gtk::Label::builder()
            .label("Suchsyntax-Operatoren")''',
        '''        let syntax_header = gtk::Label::builder()
            .label(&gettext("Search Syntax Operators"))'''
    )

    old_syntax = '''        let syntax_items = [
            ("from:", "Absender (z.B. from:alice)"),
            ("to:", "Empfänger (z.B. to:bob)"),
            ("subject:", "Betreffzeile"),
            ("body:", "Nachrichtentext"),
            ("is:unread", "Ungelesene Mails"),
            ("is:flagged", "Markierte Mails"),
            ("has:attachment", "Mails mit Anhang"),
            ("in:", "Ordner (z.B. in:sent, in:drafts)"),
            ("on:", "Datum (z.B. on:2026-08-11)"),
            ("after:", "Nach Datum (after:2026-01-01)"),
            ("before:", "Vor Datum (before:2026-12-31)"),
            ("not:", "Begriff ausschließen"),
        ];'''

    new_syntax = '''        let syntax_items: [(&'static str, String); 12] = [
            ("from:", gettext("Sender (e.g. from:alice)")),
            ("to:", gettext("Recipient (e.g. to:bob)")),
            ("subject:", gettext("Subject line")),
            ("body:", gettext("Message body")),
            ("is:unread", gettext("Unread emails")),
            ("is:flagged", gettext("Flagged emails")),
            ("has:attachment", gettext("Emails with attachment")),
            ("in:", gettext("Folder (e.g. in:sent, in:drafts)")),
            ("on:", gettext("Date (e.g. on:2026-08-11)")),
            ("after:", gettext("After date (after:2026-01-01)")),
            ("before:", gettext("Before date (before:2026-12-31)")),
            ("not:", gettext("Exclude term")),
        ];'''

    content = content.replace(old_syntax, new_syntax)
    content = content.replace('.label(desc)', '.label(&desc)')

    # 40. Refresh button, load more, batch actions
    content = content.replace(
        '''        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Nachrichten aktualisieren (F5)")
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&refresh_button, "Nachrichten aktualisieren");''',
        '''        let str_refresh = gettext("Refresh messages (F5)");
        let str_refresh_acc = gettext("Refresh messages");
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(&str_refresh)
            .css_classes(vec!["flat", "circular"])
            .build();
        set_accessible_label(&refresh_button, &str_refresh_acc);'''
    )

    content = content.replace(
        '''        let load_more_button = gtk::Button::builder()
            .label("Mehr laden")
            .tooltip_text("Weitere Nachrichten laden")
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        set_accessible_label(&load_more_button, "Weitere Nachrichten laden");''',
        '''        let str_load_more = gettext("Load More");
        let str_load_more_tip = gettext("Load more messages");
        let load_more_button = gtk::Button::builder()
            .label(&str_load_more)
            .tooltip_text(&str_load_more_tip)
            .css_classes(vec!["flat"])
            .visible(false)
            .build();
        set_accessible_label(&load_more_button, &str_load_more_tip);'''
    )

    content = content.replace(
        '''        let batch_read_button = gtk::Button::builder()
            .label("Gelesen")
            .tooltip_text("Ausgewählte Nachrichten als gelesen markieren")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_read_button = gtk::Button::builder()
            .label(&gettext("Read"))
            .tooltip_text(&gettext("Mark selected messages as read"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Gelesen-Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Mark as read not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_unread_button = gtk::Button::builder()
            .label("Ungelesen")
            .tooltip_text("Ausgewählte Nachrichten als ungelesen markieren")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_unread_button = gtk::Button::builder()
            .label(&gettext("Unread"))
            .tooltip_text(&gettext("Mark selected messages as unread"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Ungelesen-Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Mark as unread not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_flag_button = gtk::Button::builder()
            .label("Markierung")
            .tooltip_text("Markierung der ausgewählten Nachrichten umschalten")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_flag_button = gtk::Button::builder()
            .label(&gettext("Flag"))
            .tooltip_text(&gettext("Toggle flag on selected messages"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Flagging not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_important_button = gtk::Button::builder()
            .label("Wichtig")
            .tooltip_text("Wichtigkeit der ausgewählten Nachrichten umschalten")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_important_button = gtk::Button::builder()
            .label(&gettext("Important"))
            .tooltip_text(&gettext("Toggle importance on selected messages"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Wichtig-Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Importance not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_label_button = gtk::Button::builder()
            .label("Label")
            .tooltip_text("Label der ausgewählten Nachrichten setzen oder entfernen")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_label_button = gtk::Button::builder()
            .label(&gettext("Label"))
            .tooltip_text(&gettext("Set or remove label on selected messages"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''        let batch_archive_button = gtk::Button::builder()
            .label("Archivieren")
            .tooltip_text("Ausgewählte Nachrichten archivieren")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_archive_button = gtk::Button::builder()
            .label(&gettext("Archive"))
            .tooltip_text(&gettext("Archive selected messages"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Archivieren nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Archive not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_junk_button = gtk::Button::builder()
            .label("Spam")
            .tooltip_text("Ausgewählte Nachrichten als Spam markieren")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_junk_button = gtk::Button::builder()
            .label(&gettext("Junk"))
            .tooltip_text(&gettext("Mark selected messages as junk"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Spam-Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Junk marking not possible"), &error);'''
    )

    content = content.replace(
        '''        let batch_not_junk_button = gtk::Button::builder()
            .label("Nicht Spam")
            .tooltip_text("Ausgewählten Nachrichten die Spam-Markierung entziehen")
            .css_classes(vec!["flat"])
            .build();''',
        '''        let batch_not_junk_button = gtk::Button::builder()
            .label(&gettext("Not Junk"))
            .tooltip_text(&gettext("Remove junk status from selected messages"))
            .css_classes(vec!["flat"])
            .build();'''
    )

    content = content.replace(
        '''page.report_operation_error("Nicht-Spam-Markierung nicht möglich", &error);''',
        '''page.report_operation_error(&gettext("Not-junk marking not possible"), &error);'''
    )

    content = content.replace(
        '''        let move_button = gtk::MenuButton::builder()
            .label("Verschieben")
            .tooltip_text("Ausgewählte Nachrichten in einen Konto- oder lokalen Ordner verschieben")''',
        '''        let move_button = gtk::MenuButton::builder()
            .label(&gettext("Move"))
            .tooltip_text(&gettext("Move selected messages to an account or local folder"))'''
    )

    content = content.replace(
        '''        let copy_button = gtk::MenuButton::builder()
            .label("Kopieren")
            .tooltip_text("Ausgewählte Nachrichten in einen weiteren Ordner kopieren")''',
        '''        let copy_button = gtk::MenuButton::builder()
            .label(&gettext("Copy"))
            .tooltip_text(&gettext("Copy selected messages to another folder"))'''
    )

    content = content.replace(
        '''        let batch_delete_button = gtk::Button::builder()
            .label("Löschen")
            .tooltip_text("Ausgewählte Nachrichten löschen")''',
        '''        let batch_delete_button = gtk::Button::builder()
            .label(&gettext("Delete"))
            .tooltip_text(&gettext("Delete selected messages"))'''
    )

    content = content.replace(
        '''                crate::dialogs::run_destructive_action(
                    &page,
                    "Nachrichten löschen?",
                    "Die ausgewählten Nachrichten werden dauerhaft gelöscht bzw. in den Papierkorb verschoben.",
                    "Löschen",''',
        '''                let heading = gettext("Delete messages?");
                let body = gettext("The selected messages will be permanently deleted or moved to trash.");
                let del_lbl = gettext("Delete");
                crate::dialogs::run_destructive_action(
                    &page,
                    &heading,
                    &body,
                    &del_lbl,'''
    )

    content = content.replace(
        '''page_for_action.report_operation_error("Löschen nicht möglich", &error);''',
        '''page_for_action.report_operation_error(&gettext("Delete not possible"), &error);'''
    )

    # 41. Modern details button and action group
    content = content.replace(
        '''        let modern_details_button = gtk::Button::builder()
            .label("Details ▾")
            .tooltip_text("Ausführliche Kopfzeilen anzeigen")''',
        '''        let modern_details_button = gtk::Button::builder()
            .label(&gettext("Details ▾"))
            .tooltip_text(&gettext("Show detailed headers"))'''
    )

    content = content.replace(
        '''        let modern_star_button = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text("Nachricht markieren")
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&modern_star_button, "Nachricht markieren");''',
        '''        let str_star_tip = gettext("Star message");
        let modern_star_button = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text(&str_star_tip)
            .css_classes(vec!["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&modern_star_button, &str_star_tip);'''
    )

    content = content.replace(
        '''        let m_reply = make_header_action_btn("mail-reply-sender-symbolic", "Antworten (Strg+R)", "win.reply");
        let m_reply_all = make_header_action_btn("mail-reply-all-symbolic", "Allen antworten (Strg+Umschalt+R)", "win.reply-all");
        let m_forward = make_header_action_btn("mail-forward-symbolic", "Weiterleiten (Strg+L)", "win.forward");

        let m_more_menu = gio::Menu::new();
        m_more_menu.append(Some("Als EML exportieren …"), Some("win.export"));
        m_more_menu.append(Some("Nachrichtenquelle anzeigen"), Some("win.view-message-source"));
        m_more_menu.append(Some("Helles Design für diese E-Mail"), Some("win.toggle-mail-light-mode"));

        let m_more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&m_more_menu)
            .tooltip_text("Weitere Aktionen für diese Nachricht")
            .css_classes(vec!["flat", "circular", "mail-header-action-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&m_more_btn, "Weitere Aktionen für diese Nachricht");''',
        '''        let str_m_reply = gettext("Reply (Ctrl+R)");
        let str_m_reply_all = gettext("Reply All (Ctrl+Shift+R)");
        let str_m_forward = gettext("Forward (Ctrl+L)");
        let m_reply = make_header_action_btn("mail-reply-sender-symbolic", &str_m_reply, "win.reply");
        let m_reply_all = make_header_action_btn("mail-reply-all-symbolic", &str_m_reply_all, "win.reply-all");
        let m_forward = make_header_action_btn("mail-forward-symbolic", &str_m_forward, "win.forward");

        let m_more_menu = gio::Menu::new();
        let str_export = gettext("Save as EML…");
        let str_view_src = gettext("View Message Source");
        let str_light_mode = gettext("Light theme for this email");
        m_more_menu.append(Some(&str_export), Some("win.export"));
        m_more_menu.append(Some(&str_view_src), Some("win.view-message-source"));
        m_more_menu.append(Some(&str_light_mode), Some("win.toggle-mail-light-mode"));

        let str_more_tip = gettext("More actions for this message");
        let m_more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&m_more_menu)
            .tooltip_text(&str_more_tip)
            .css_classes(vec!["flat", "circular", "mail-header-action-btn"])
            .valign(gtk::Align::Center)
            .build();
        set_accessible_label(&m_more_btn, &str_more_tip);'''
    )

    content = content.replace(
        '''        let preview_more_menu = gio::Menu::new();
        preview_more_menu.append(Some("Als EML exportieren …"), Some("win.export"));
        preview_more_menu.append(Some("Nachrichtenquelle anzeigen"), Some("win.view-message-source"));
        preview_more_menu.append(Some("Helles Design für diese E-Mail"), Some("win.toggle-mail-light-mode"));

        let preview_more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&preview_more_menu)
            .tooltip_text("Weitere Aktionen für diese Nachricht")''',
        '''        let preview_more_menu = gio::Menu::new();
        let str_prev_export = gettext("Save as EML…");
        let str_prev_src = gettext("View Message Source");
        let str_prev_light = gettext("Light theme for this email");
        preview_more_menu.append(Some(&str_prev_export), Some("win.export"));
        preview_more_menu.append(Some(&str_prev_src), Some("win.view-message-source"));
        preview_more_menu.append(Some(&str_prev_light), Some("win.toggle-mail-light-mode"));

        let str_prev_more = gettext("More actions for this message");
        let preview_more_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&preview_more_menu)
            .tooltip_text(&str_prev_more)'''
    )

    # 42. set_graph_local_search_warning and load cancelled
    content = content.replace(
        '''                            "Mail-Ladevorgang wurde abgebrochen.".to_string(),''',
        '''                            gettext("Mail loading was cancelled."),'''
    )

    content = content.replace(
        '''    fn set_graph_local_search_warning(&self, warning: Option<&str>) {
        if let Some(label) = self.imp().graph_local_search_warning.borrow().as_ref() {
            label.set_label(warning.unwrap_or_default());
            label.set_visible(warning.is_some());
        }
    }''',
        '''    fn set_graph_local_search_warning(&self, warning: Option<&str>) {
        if let Some(label) = self.imp().graph_local_search_warning.borrow().as_ref() {
            let text = warning.map(gettext).unwrap_or_default();
            label.set_label(&text);
            label.set_visible(warning.is_some());
        }
    }'''
    )

    # 43. Load apply status
    content = content.replace(
        '''                    state.set_label(&format!("Kontosynchronisation nicht möglich: {error}"));''',
        '''                    state.set_label(&gettext("Account synchronization not possible: {error}").replace("{error}", &error));'''
    )

    content = content.replace(
        '''            let prefix = pending
                .camel_status
                .as_deref()
                .or(pending.local_reason.as_deref())
                .unwrap_or("Nachrichten");
            state.set_label(&format!(
                "{prefix} · {} Nachricht(en) aufgebaut …",
                pending.appended
            ));''',
        '''            let fallback_msgs = gettext("Messages");
            let prefix = pending
                .camel_status
                .as_deref()
                .or(pending.local_reason.as_deref())
                .unwrap_or(&fallback_msgs);
            let count_str = ngettext(
                "{} message built…",
                "{} messages built…",
                pending.appended as u32,
            ).replace("{}", &pending.appended.to_string());
            state.set_label(&format!("{prefix} · {count_str}"));'''
    )

    content = content.replace(
        '''                let reason = pending
                    .local_reason
                    .as_deref()
                    .unwrap_or("Lokaler Speicher");
                state.set_label(&format!(
                    "{reason} · {} lokale Nachricht(en) geladen",
                    pending.appended
                ));''',
        '''                let fallback_storage = gettext("Local Storage");
                let reason = pending
                    .local_reason
                    .as_deref()
                    .unwrap_or(&fallback_storage);
                let count_str = ngettext(
                    "{} local message loaded",
                    "{} local messages loaded",
                    pending.appended as u32,
                ).replace("{}", &pending.appended.to_string());
                state.set_label(&format!("{reason} · {count_str}"));'''
    )

    content = content.replace(
        '''                state.set_label(&format!(
                    "{} · {} lokale Nachricht(en) geladen",
                    pending
                        .local_reason
                        .as_deref()
                        .unwrap_or("Lokaler Speicher"),
                    pending.appended
                ));''',
        '''                let fallback_storage = gettext("Local Storage");
                let reason = pending
                    .local_reason
                    .as_deref()
                    .unwrap_or(&fallback_storage);
                let count_str = ngettext(
                    "{} local message loaded",
                    "{} local messages loaded",
                    pending.appended as u32,
                ).replace("{}", &pending.appended.to_string());
                state.set_label(&format!("{reason} · {count_str}"));'''
    )

    content = content.replace(
        '''                state.set_label(&format!(
                    "{} · {} Nachricht(en) in dieser Seite",
                    pending.camel_status.as_deref().unwrap_or("Mailbackend"),
                    pending.appended
                ));''',
        '''                let fallback_backend = gettext("Mail Backend");
                let status = pending.camel_status.as_deref().unwrap_or(&fallback_backend);
                let count_str = ngettext(
                    "{} message on this page",
                    "{} messages on this page",
                    pending.appended as u32,
                ).replace("{}", &pending.appended.to_string());
                state.set_label(&format!("{status} · {count_str}"));'''
    )

    # 44. iTIP invitation widgets
    content = content.replace(
        '''        details.push(format!("Organisator: {}", invitation.organizer));''',
        '''        details.push(format!("{}: {}", gettext("Organizer"), invitation.organizer));'''
    )

    content = content.replace(
        '''        let import_button = gtk::Button::builder()
            .label("Lokal übernehmen")
            .tooltip_text("Diese Einladung in den app-eigenen Kalender übernehmen")''',
        '''        let import_button = gtk::Button::builder()
            .label(&gettext("Import Locally"))
            .tooltip_text(&gettext("Import this invitation into the app-owned calendar"))'''
    )

    content = content.replace(
        '''                        if let Some(button) = button_weak.upgrade() {
                            button.set_label("Übernommen");
                        }
                        show_mail_toast_for_widget(
                            &row_for_future,
                            &format!("Termin lokal übernommen ({})", result.uid),
                        );''',
        '''                        if let Some(button) = button_weak.upgrade() {
                            button.set_label(&gettext("Imported"));
                        }
                        show_mail_toast_for_widget(
                            &row_for_future,
                            &gettext("Event imported locally ({id})").replace("{id}", &result.uid),
                        );'''
    )

    content = content.replace(
        '''                        show_mail_toast_for_widget(
                            &row_for_future,
                            &format!("Einladung konnte nicht übernommen werden: {error}"),
                        );''',
        '''                        show_mail_toast_for_widget(
                            &row_for_future,
                            &gettext("Invitation could not be imported: {error}").replace("{error}", &error.to_string()),
                        );'''
    )

    content = content.replace(
        '''            for (label, status) in [
                ("Zusagen", ItipReplyStatus::Accepted),
                ("Vorbehalt", ItipReplyStatus::Tentative),
                ("Ablehnen", ItipReplyStatus::Declined),
            ] {
                let reply_button = gtk::Button::builder()
                    .label(label)
                    .tooltip_text(&format!("iTIP-Antwort: {}", status.as_str()))''',
        '''            for (label, status) in [
                (gettext("Accept"), ItipReplyStatus::Accepted),
                (gettext("Tentative"), ItipReplyStatus::Tentative),
                (gettext("Decline"), ItipReplyStatus::Declined),
            ] {
                let reply_button = gtk::Button::builder()
                    .label(&label)
                    .tooltip_text(&format!("{}: {}", gettext("iTIP Response"), status.as_str()))'''
    )

    content = content.replace(
        '''        let status = gtk::Label::builder()
            .label("Kontosynchronisation erforderlich")''',
        '''        let status = gtk::Label::builder()
            .label(&gettext("Account synchronization required"))'''
    )

    content = content.replace(
        '''                    if let Some(button) = button_weak.upgrade() {
                        button.set_label("Entwurf gespeichert");
                    }
                    show_mail_toast_for_widget(
                        &row_for_future,
                        &format!("iTIP-Antwort als Entwurf gespeichert ({draft_id})"),
                    );''',
        '''                    if let Some(button) = button_weak.upgrade() {
                        button.set_label(&gettext("Draft saved"));
                    }
                    show_mail_toast_for_widget(
                        &row_for_future,
                        &gettext("iTIP response saved as draft ({draft_id})").replace("{draft_id}", &draft_id),
                    );'''
    )

    content = content.replace(
        '''                    show_mail_toast_for_widget(
                        &row_for_future,
                        &format!("iTIP-Antwort konnte nicht gespeichert werden: {error}"),
                    );''',
        '''                    show_mail_toast_for_widget(
                        &row_for_future,
                        &gettext("iTIP response could not be saved: {error}").replace("{error}", &error.to_string()),
                    );'''
    )

    content = content.replace(
        '''            "Kein aktiviertes lokales Kontoprofil für die Antwort vorhanden".to_string()''',
        '''            gettext("No enabled local account profile available for reply")'''
    )

    content = content.replace(
        '''        "Das ausgewählte Kontoprofil besitzt keine gültige E-Mail-Adresse".to_string()''',
        '''        gettext("The selected account profile does not have a valid email address")'''
    )

    content = content.replace(
        '''            .ok_or_else(|| "Die Einladung besitzt keinen gültigen Organisator".to_string())?;''',
        '''            .ok_or_else(|| gettext("The invitation does not have a valid organizer"))?;'''
    )

    content = content.replace(
        '''        "Kalendereinladung"''',
        '''        &gettext("Calendar Invitation")'''
    )

    content = content.replace(
        '''        html_body: format!(
            "{}: {}\n\nDie iTIP-Antwort wurde als text/calendar-Entwurf vorbereitet.",
            status.as_str(),
            summary
        ),''',
        '''        html_body: format!(
            "{}: {}\n\n{}",
            status.as_str(),
            summary,
            gettext("The iTIP reply was prepared as a text/calendar draft.")
        ),'''
    )

    # 45. Tests updates
    content = content.replace(
        '''        assert!(status.contains("1 Konto nicht synchronisiert"));''',
        '''        assert!(status.contains(&gettext("{} account not synchronized").replace("{}", "1")));'''
    )

    content = content.replace(
        '''        assert!(status.contains("1 Konto synchronisiert"));
        assert!(status.contains("Keine lesbaren Mailordner gefunden"));''',
        '''        assert!(status.contains(&gettext("{} account synchronized").replace("{}", "1")));
        assert!(status.contains(&gettext("No readable mail folders found")));'''
    )

    content = content.replace(
        '''        assert_eq!(
            mail_message_location_label("custom:Projects/2026", None, &[]),
            "Lokale Ordner / 2026"
        );''',
        '''        assert_eq!(
            mail_message_location_label("custom:Projects/2026", None, &[]),
            format!("{} / 2026", gettext("Local Folders"))
        );'''
    )

    content = content.replace(
        '''            vec![
                ("Betreff".to_string(), "Roadmap".to_string()),
                ("Received".to_string(), "first".to_string()),
                ("Received".to_string(), "second".to_string()),
                ("Mailer".to_string(), "Evolution".to_string()),
            ]''',
        '''            vec![
                (gettext("Subject"), "Roadmap".to_string()),
                ("Received".to_string(), "first".to_string()),
                ("Received".to_string(), "second".to_string()),
                (gettext("Mailer"), "Evolution".to_string()),
            ]'''
    )

    content = content.replace(
        '''        assert_eq!(
            mail_row_status_icon(true, false, false),
            ("media-record-symbolic", "Ungelesen")
        );
        assert_eq!(
            mail_row_status_icon(false, false, false),
            ("", "Gelesen")
        );
        assert_eq!(
            mail_row_status_icon(false, true, false),
            ("mail-replied-symbolic", "Beantwortet")
        );
        assert_eq!(
            mail_row_status_icon(false, false, true),
            ("mail-forwarded-symbolic", "Weitergeleitet")
        );
        assert_eq!(
            mail_row_status_icon(false, true, true),
            ("mail-replied-symbolic", "Beantwortet und weitergeleitet")
        );''',
        '''        assert_eq!(
            mail_row_status_icon(true, false, false),
            ("media-record-symbolic", gettext("Unread"))
        );
        assert_eq!(
            mail_row_status_icon(false, false, false),
            ("", gettext("Read"))
        );
        assert_eq!(
            mail_row_status_icon(false, true, false),
            ("mail-replied-symbolic", gettext("Replied"))
        );
        assert_eq!(
            mail_row_status_icon(false, false, true),
            ("mail-forwarded-symbolic", gettext("Forwarded"))
        );
        assert_eq!(
            mail_row_status_icon(false, true, true),
            ("mail-replied-symbolic", gettext("Replied and forwarded"))
        );'''
    )

    content = content.replace(
        '''        assert_eq!(
            mail_row_attachment_info(&[cal_attachment]),
            Some(("x-office-calendar-symbolic", "Termineinladung (.ics)"))
        );''',
        '''        assert_eq!(
            mail_row_attachment_info(&[cal_attachment]),
            Some(("x-office-calendar-symbolic", gettext("Calendar invitation (.ics)")))
        );'''
    )

    content = content.replace(
        '''        assert_eq!(
            mail_row_attachment_info(&[vcard_attachment]),
            Some(("x-office-address-book-symbolic", "Kontaktkarte (.vcf)"))
        );''',
        '''        assert_eq!(
            mail_row_attachment_info(&[vcard_attachment]),
            Some(("x-office-address-book-symbolic", gettext("Contact card (.vcf)")))
        );'''
    )

    content = content.replace(
        '''        assert_eq!(
            mail_row_attachment_info(&[file_attachment]),
            Some(("mail-attachment-symbolic", "Anhang"))
        );''',
        '''        assert_eq!(
            mail_row_attachment_info(&[file_attachment]),
            Some(("mail-attachment-symbolic", gettext("Attachment")))
        );'''
    )

    content = content.replace(
        '''        assert_eq!(local.folder_name, "Posteingang");
        assert_eq!((local.unread_count, local.total_count), (3, 8));
        assert_eq!(local.folder_path, "Lokale Ordner / Posteingang");''',
        '''        assert_eq!(local.folder_name, gettext("Inbox"));
        assert_eq!((local.unread_count, local.total_count), (3, 8));
        assert_eq!(local.folder_path, format!("{} / {}", gettext("Local Folders"), gettext("Inbox")));'''
    )

    content = content.replace(
        '''            Some("Lokale Ordner / Projects/2026".to_string())''',
        '''            Some(format!("{} / Projects/2026", gettext("Local Folders")))'''
    )

    content = content.replace(
        '''        assert!(inbox_title == "Inbox" || inbox_title == "Posteingang");''',
        '''        assert_eq!(inbox_title, crate::i18n::gettext("Inbox"));'''
    )
    content = content.replace(
        '''        assert!(sent_title == "Sent" || sent_title == "Gesendet");''',
        '''        assert_eq!(sent_title, crate::i18n::gettext("Sent"));'''
    )
    content = content.replace(
        '''        assert!(drafts_title == "Drafts" || drafts_title == "Entwürfe");''',
        '''        assert_eq!(drafts_title, crate::i18n::gettext("Drafts"));'''
    )
    content = content.replace(
        '''        assert!(trash_title == "Trash" || trash_title == "Papierkorb");''',
        '''        assert_eq!(trash_title, crate::i18n::gettext("Trash"));'''
    )
    content = content.replace(
        '''        assert!(archive_title == "Archive" || archive_title == "Archiv" || archive_title == "Archivieren");''',
        '''        assert_eq!(archive_title, crate::i18n::gettext("Archive"));'''
    )
    content = content.replace(
        '''        assert!(spam_title == "Junk" || spam_title == "Spam");''',
        '''        assert_eq!(spam_title, crate::i18n::gettext("Junk"));'''
    )
    content = content.replace(
        '''        assert!(outbox_title == "Outbox" || outbox_title == "Postausgang");''',
        '''        assert_eq!(outbox_title, crate::i18n::gettext("Outbox"));'''
    )

    content = content.replace(
        '''        let t_inbox = MailPage::clean_folder_display_title("INBOX");
        assert!(t_inbox == "Inbox" || t_inbox == "Posteingang");
        let t_inbox_lower = MailPage::clean_folder_display_title("inbox");
        assert!(t_inbox_lower == "Inbox" || t_inbox_lower == "Posteingang");
        let t_post = MailPage::clean_folder_display_title("Posteingang");
        assert!(t_post == "Inbox" || t_post == "Posteingang");''',
        '''        let t_inbox = MailPage::clean_folder_display_title("INBOX");
        assert_eq!(t_inbox, crate::i18n::gettext("Inbox"));
        let t_inbox_lower = MailPage::clean_folder_display_title("inbox");
        assert_eq!(t_inbox_lower, crate::i18n::gettext("Inbox"));
        let t_post = MailPage::clean_folder_display_title("Posteingang");
        assert_eq!(t_post, crate::i18n::gettext("Inbox"));'''
    )

    content = content.replace(
        '''        assert!(markup_empty.contains("(Kein Betreff)"));''',
        '''        assert!(markup_empty.contains(&gettext("(No Subject)")));'''
    )

    old_test_drawer_tooltip = '''    #[test]
    fn test_format_drawer_folder_tooltip() {
        let inbox = FolderSelection::from_folder_id("inbox");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, "Posteingang", 3, 102),
            "Posteingang: 3 ungelesen, 102 gesamt"
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, "Posteingang", 1, 102),
            "Posteingang: 1 ungelesen, 102 gesamt"
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, "Posteingang", 0, 102),
            "Posteingang: 102 E-Mails"
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, "Posteingang", 0, 1),
            "Posteingang: 1 E-Mail"
        );
        let empty_tip = MailPage::format_drawer_folder_tooltip(&inbox, "Posteingang", 0, 0);
        assert!(
            empty_tip == "Posteingang (No emails)" || empty_tip == "Posteingang (Keine E-Mails)",
            "Expected localized empty tooltip, got: {empty_tip}"
        );

        let sent = FolderSelection::from_folder_id("sent");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&sent, "Gesendet", 0, 55),
            "Gesendet: 55 gesendete E-Mails"
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&sent, "Gesendet", 0, 1),
            "Gesendet: 1 gesendete E-Mail"
        );

        let drafts = FolderSelection::from_folder_id("drafts");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&drafts, "Entwürfe", 0, 4),
            "Entwürfe: 4 Entwürfe"
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&drafts, "Entwürfe", 0, 1),
            "Entwürfe: 1 Entwurf"
        );

        let trash = FolderSelection::from_folder_id("trash");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&trash, "Papierkorb", 0, 12),
            "Papierkorb: 12 gelöschte E-Mails"
        );

        let spam = FolderSelection::from_folder_id("spam");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&spam, "Junk", 0, 8),
            "Junk: 8 unerwünschte E-Mails"
        );

        let mut custom = FolderSelection::from_folder_id("sent");
        custom.folder_name = Some("[Gmail]/Gesendet".to_string());
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&custom, "Gesendet", 0, 55),
            "Gesendet ([Gmail]/Gesendet): 55 gesendete E-Mails"
        );
    }'''

    new_test_drawer_tooltip = '''    #[test]
    fn test_format_drawer_folder_tooltip() {
        let inbox = FolderSelection::from_folder_id("inbox");
        let inbox_label = gettext("Inbox");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, &inbox_label, 3, 102),
            format!("{}: {}, {}", inbox_label, ngettext("1 unread", "{} unread", 3).replace("{}", "3"), ngettext("1 total", "{} total", 102).replace("{}", "102"))
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, &inbox_label, 1, 102),
            format!("{}: {}, {}", inbox_label, ngettext("1 unread", "{} unread", 1).replace("{}", "1"), ngettext("1 total", "{} total", 102).replace("{}", "102"))
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, &inbox_label, 0, 102),
            format!("{}: {}", inbox_label, ngettext("{} email", "{} emails", 102).replace("{}", "102"))
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&inbox, &inbox_label, 0, 1),
            format!("{}: {}", inbox_label, gettext("1 email"))
        );
        let empty_tip = MailPage::format_drawer_folder_tooltip(&inbox, &inbox_label, 0, 0);
        assert_eq!(empty_tip, format!("{inbox_label} ({})", gettext("No emails")));

        let sent = FolderSelection::from_folder_id("sent");
        let sent_label = gettext("Sent");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&sent, &sent_label, 0, 55),
            format!("{}: {}", sent_label, ngettext("1 sent email", "{} sent emails", 55).replace("{}", "55"))
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&sent, &sent_label, 0, 1),
            format!("{}: {}", sent_label, ngettext("1 sent email", "{} sent emails", 1).replace("{}", "1"))
        );

        let drafts = FolderSelection::from_folder_id("drafts");
        let drafts_label = gettext("Drafts");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&drafts, &drafts_label, 0, 4),
            format!("{}: {}", drafts_label, ngettext("1 draft", "{} drafts", 4).replace("{}", "4"))
        );
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&drafts, &drafts_label, 0, 1),
            format!("{}: {}", drafts_label, ngettext("1 draft", "{} drafts", 1).replace("{}", "1"))
        );

        let trash = FolderSelection::from_folder_id("trash");
        let trash_label = gettext("Trash");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&trash, &trash_label, 0, 12),
            format!("{}: {}", trash_label, ngettext("1 deleted email", "{} deleted emails", 12).replace("{}", "12"))
        );

        let spam = FolderSelection::from_folder_id("spam");
        let spam_label = gettext("Junk");
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&spam, &spam_label, 0, 8),
            format!("{}: {}", spam_label, ngettext("1 junk email", "{} junk emails", 8).replace("{}", "8"))
        );

        let mut custom = FolderSelection::from_folder_id("sent");
        custom.folder_name = Some("[Gmail]/Gesendet".to_string());
        assert_eq!(
            MailPage::format_drawer_folder_tooltip(&custom, &sent_label, 0, 55),
            format!("{sent_label} ([Gmail]/Gesendet): {}", ngettext("1 sent email", "{} sent emails", 55).replace("{}", "55"))
        );
    }'''

    content = content.replace(old_test_drawer_tooltip, new_test_drawer_tooltip)

    # 46. Update test_format_month_short_de to test_format_month_short
    old_test_month = '''    #[test]
    fn test_format_month_short_de() {
        assert_eq!(format_month_short_de(1), "Jan.");
        assert_eq!(format_month_short_de(2), "Feb.");
        assert_eq!(format_month_short_de(3), "März");
        assert_eq!(format_month_short_de(4), "Apr.");
        assert_eq!(format_month_short_de(5), "Mai");
        assert_eq!(format_month_short_de(6), "Juni");
        assert_eq!(format_month_short_de(7), "Juli");
        assert_eq!(format_month_short_de(8), "Aug.");
        assert_eq!(format_month_short_de(9), "Sept.");
        assert_eq!(format_month_short_de(10), "Okt.");
        assert_eq!(format_month_short_de(11), "Nov.");
        assert_eq!(format_month_short_de(12), "Dez.");
        assert_eq!(format_month_short_de(13), "");
    }'''

    new_test_month = '''    #[test]
    fn test_format_month_short() {
        assert_eq!(format_month_short(1), crate::i18n::gettext("Jan"));
        assert_eq!(format_month_short(2), crate::i18n::gettext("Feb"));
        assert_eq!(format_month_short(3), crate::i18n::gettext("Mar"));
        assert_eq!(format_month_short(4), crate::i18n::gettext("Apr"));
        assert_eq!(format_month_short(5), crate::i18n::gettext("May"));
        assert_eq!(format_month_short(6), crate::i18n::gettext("Jun"));
        assert_eq!(format_month_short(7), crate::i18n::gettext("Jul"));
        assert_eq!(format_month_short(8), crate::i18n::gettext("Aug"));
        assert_eq!(format_month_short(9), crate::i18n::gettext("Sep"));
        assert_eq!(format_month_short(10), crate::i18n::gettext("Oct"));
        assert_eq!(format_month_short(11), crate::i18n::gettext("Nov"));
        assert_eq!(format_month_short(12), crate::i18n::gettext("Dec"));
        assert_eq!(format_month_short(13), "");
    }'''

    content = content.replace(old_test_month, new_test_month)

    # 47. Test labels size group
    content = content.replace(
        '''        let label1 = gtk::Label::new(Some("Antworten"));
        let label2 = gtk::Label::new(Some("Allen antworten"));
        let label3 = gtk::Label::new(Some("Weiterleiten"));''',
        '''        let label1 = gtk::Label::new(Some("Reply"));
        let label2 = gtk::Label::new(Some("Reply to All"));
        let label3 = gtk::Label::new(Some("Forward"));'''
    )

    # 48. Test toolbar buttons desktop mode
    content = content.replace(
        '''        assert!(label.label() == "Print" || label.label() == "Drucken");''',
        '''        assert_eq!(label.label(), crate::i18n::gettext("Print"));'''
    )

    content = content.replace(
        '''        assert!(label.label() == "Delete" || label.label() == "Löschen");''',
        '''        assert_eq!(label.label(), crate::i18n::gettext("Delete"));'''
    )

    return content

if __name__ == "__main__":
    path = "src/views/mail_page.rs"
    with open(path, "r", encoding="utf-8") as f:
        orig = f.read()
    migrated = migrate(orig)
    if orig == migrated:
        print("No changes made!", file=sys.stderr)
        sys.exit(1)
    with open(path, "w", encoding="utf-8") as f:
        f.write(migrated)
    print("Migration of mail_page.rs complete.")
