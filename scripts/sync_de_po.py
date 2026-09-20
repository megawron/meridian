#!/usr/bin/env python3
"""
Sync po/evolution-gtk4.pot and po/de.po with all extracted and migrated strings.
"""
import os
import sys
import subprocess
import re

BASE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PO_DIR = os.path.join(BASE_DIR, "po")
POT_PATH = os.path.join(PO_DIR, "evolution-gtk4.pot")
DE_PO_PATH = os.path.join(PO_DIR, "de.po")
COMPENDIUM = os.path.join(os.path.dirname(BASE_DIR), "evolution", "po", "de.po")
LOCALEDIR_MO = os.path.join(BASE_DIR, "data", "locale", "de", "LC_MESSAGES", "evolution-gtk4.mo")
MIG_PATH = os.path.join(BASE_DIR, "scripts", "migrate_mail_page.py")

def main():
    # 1. Update POT file from source files
    subprocess.run(["python3", os.path.join(BASE_DIR, "scripts", "update_po.py")], check=True)

    # 2. Curated dictionary of manual & migrated translations
    manual = {
        "Preview Pane": "Vorschaufenster",
        "Bottom": "Unten",
        "Classic view: preview below message list": "Klassische Ansicht: Vorschau unter der Nachrichtenliste",
        "Right": "Rechts",
        "Modern view: preview to the right of message list": "Moderne Ansicht: Vorschau rechts neben der Nachrichtenliste",
        "Layout Profile": "Darstellungsprofil",
        "Compact, multi-line email list": "Kompakte, mehrzeilige E-Mail-Liste",
        "Wide": "Breit",
        "Tabular list with columns": "Tabellarische Liste mit Spalten",
        "Cards": "Karten",
        "Spacious card view (Gmail / Outlook style)": "Großzügige Kartenansicht (Gmail- / Outlook-Stil)",
        "Header Style": "Kopfzeilen-Stil",
        "Modern": "Modern",
        "Webmail style with avatar, prominent sender, and collapsible details": "Webmail-Stil mit Avatar, hervorgehobenem Absender und einklappbaren Details",
        "Classic": "Klassisch",
        "Traditional tabular headers (GNOME Evolution)": "Klassische tabellarische Kopfzeilen (GNOME Evolution)",
        "Grouping & Display": "Gruppierung & Anzeige",
        "Avatars": "Avatare",
        "Sender": "Absender",
        "Recipient": "Empfänger",
        "Date": "Datum",
        "Attachments": "Anhänge",
        "Preview text": "Vorschautext",
        "Labels": "Beschriftungen",
        "Size": "Größe",
        "Due by": "Fällig am",
        "Location": "Ort",
        "Group by conversations": "Nach Unterhaltungen gruppieren",
        "Group related emails into discussion threads": "Zusammenhängende E-Mails in Diskussionsfäden gruppieren",
        "Show deleted messages": "Gelöschte Nachrichten anzeigen",
        "Leave emails marked for deletion struck through in the list": "Als gelöscht markierte E-Mails durchgestrichen in der Liste belassen",
        "Visible Columns": "Sichtbare Spalten",
        "Customize column order…": "Spaltenreihenfolge anpassen …",
        "Show status actions in toolbar": "Status-Aktionen in Werkzeugleiste anzeigen",
        "Show tasks in agenda": "Aufgaben in der Agenda anzeigen",
        "Customize Column Order": "Spaltenreihenfolge anpassen",
        "Reverse sort order": "Sortierreihenfolge umkehren",
        "Restore default columns": "Standardspalten wiederherstellen",
        "Settings are not available in this development mode.": "Einstellungen sind in diesem Entwicklungsmodus nicht verfügbar.",
        "View options": "Ansichtsoptionen",
        "Search Options & Filters": "Suchoptionen und Filter",
        "Search field:": "Suchfeld:",
        "Search scope:": "Suchbereich:",
        "Filter:": "Filter:",
        "Search Syntax Operators": "Suchsyntax-Operatoren",
        "Sender (e.g. from:alice)": "Absender (z.B. from:alice)",
        "Recipient (e.g. to:bob)": "Empfänger (z.B. to:bob)",
        "Subject line": "Betreffzeile",
        "Message body": "Nachrichtentext",
        "Unread emails": "Ungelesene E-Mails",
        "Flagged emails": "Markierte E-Mails",
        "Emails with attachment": "E-Mails mit Anhang",
        "Folder (e.g. in:sent, in:drafts)": "Ordner (z.B. in:sent, in:drafts)",
        "Date (e.g. on:2026-08-11)": "Datum (z.B. on:2026-08-11)",
        "After date (after:2026-01-01)": "Nach Datum (after:2026-01-01)",
        "Before date (before:2026-12-31)": "Vor Datum (before:2026-12-31)",
        "Exclude term": "Begriff ausschließen",
        "Save as EML…": "Als EML exportieren …",
        "View Message Source": "Nachrichtenquelle anzeigen",
        "Light theme for this email": "Helles Design für diese E-Mail",
        "More actions for this message": "Weitere Aktionen für diese Nachricht",
        "Toggle Flag (Star)": "Markierung umschalten (Stern)",
        "Toggle Importance": "Wichtigkeitsmarkierung umschalten",
        "Account not available": "Konto nicht verfügbar",
        "All": "Alle",
        "Current Folder": "Aktueller Ordner",
        "Current Account": "Aktuelles Konto",
        "All Accounts": "Alle Konten",
        "Unread": "Ungelesen",
        "Flagged": "Markiert",
        "Important": "Wichtig",
        "With Attachments": "Mit Anhängen",
        "Last 7 Days": "Letzte 7 Tage",
        "Last 30 Days": "Letzte 30 Tage",
        "Mark as Read": "Als gelesen markieren",
        "Mark as Unread": "Als ungelesen markieren",
        "Mark as Junk": "Als Spam markieren",
        "Mark as Not Junk": "Als Nicht-Spam markieren",
        "Replied and forwarded": "Beantwortet und weitergeleitet",
        "Replied": "Beantwortet",
        "Forwarded": "Weitergeleitet",
        "Read": "Gelesen",
        "Calendar invitation (.ics)": "Termineinladung (.ics)",
        "Contact card (.vcf)": "Kontaktkarte (.vcf)",
        "Attachment": "Anhang",
        "(No Subject)": "(Kein Betreff)",
        "From": "Von",
        "Reply-To": "Antwort an",
        "To": "An",
        "Cc": "Cc",
        "Bcc": "Bcc",
        "Subject": "Betreff",
        "Newsgroups": "Newsgroups",
        "Face": "Face",
        "Mailer": "Mailer",
        "Sender picture could not be displayed": "Absenderbild konnte nicht angezeigt werden",
        "No message displayed": "Keine Nachricht angezeigt",
        "Searching…": "Suchen…",
        "Matches": "Treffer",
        "Inbox": "Posteingang",
        "Sent": "Gesendet",
        "Drafts": "Entwürfe",
        "Trash": "Papierkorb",
        "Archive": "Archiv",
        "Junk": "Spam",
        "Local Folders": "Lokale Ordner",
        "Note": "Hinweis",
        "No readable mail folders found": "Keine lesbaren E-Mail-Ordner gefunden",
        "More actions": "Weitere Aktionen",
        "Refresh messages (F5)": "Nachrichten aktualisieren (F5)",
        "Refresh messages": "Nachrichten aktualisieren",
        "Load More": "Mehr laden",
        "Load more messages": "Weitere Nachrichten laden",
        "Mark selected messages as read": "Ausgewählte Nachrichten als gelesen markieren",
        "Mark selected messages as unread": "Ausgewählte Nachrichten als ungelesen markieren",
        "Toggle flag on selected messages": "Markierung für ausgewählte Nachrichten umschalten",
        "Toggle importance on selected messages": "Wichtigkeit für ausgewählte Nachrichten umschalten",
        "Archive selected messages": "Ausgewählte Nachrichten archivieren",
        "Mark selected messages as junk": "Ausgewählte Nachrichten als unerwünscht markieren",
        "Remove junk status from selected messages": "Unerwünscht-Status von ausgewählten Nachrichten entfernen",
        "Move selected messages to an account or local folder": "Ausgewählte Nachrichten in ein Konto oder einen lokalen Ordner verschieben",
        "Copy selected messages to an account or local folder": "Ausgewählte Nachrichten in ein Konto oder einen lokalen Ordner kopieren",
        "Delete selected messages": "Ausgewählte Nachrichten löschen",
        "Empty Trash": "Papierkorb leeren",
        "Select All": "Alle auswählen",
        "Select None": "Keine auswählen",
    }

    pairs = dict(manual)

    # 3. Extract pairs from migration script if available
    if os.path.exists(MIG_PATH):
        with open(MIG_PATH, "r", encoding="utf-8") as f:
            mig_text = f.read()
        matches = re.findall(r"content\.replace\(\s*'\''(.*?)'\''\s*,\s*'\''(.*?)'\''\s*\)", mig_text, re.DOTALL)
        gettext_re = re.compile(r"gettext\(\"([^\"]+)\"\)")
        for old_code, new_code in matches:
            old_lines = old_code.strip().splitlines()
            new_lines = new_code.strip().splitlines()
            if len(old_lines) == len(new_lines):
                for o_line, n_line in zip(old_lines, new_lines):
                    eng_m = gettext_re.findall(n_line)
                    if len(eng_m) == 1:
                        de_m = re.findall(r"\"([^\"]+)\"", o_line)
                        de_cand = [s for s in de_m if not s.startswith("win.") and "symbolic" not in s and s not in ["flat", "circular", "suggested-action", "destructive-action", "dim-label"]]
                        if len(de_cand) == 1 and eng_m[0] not in pairs:
                            pairs[eng_m[0]] = de_cand[0]

    po_lines = [
        "msgid \"\"",
        "msgstr \"\"",
        "\"Content-Type: text/plain; charset=UTF-8\\n\"",
        ""
    ]

    for msgid, msgstr in pairs.items():
        esc_id = msgid.replace("\\", "\\\\").replace("\"", "\\\"")
        esc_str = msgstr.replace("\\", "\\\\").replace("\"", "\\\"")
        po_lines.append(f"msgid \"{esc_id}\"")
        po_lines.append(f"msgstr \"{esc_str}\"")
        po_lines.append("")

    curated_path = "/tmp/curated_de.po"
    with open(curated_path, "w", encoding="utf-8") as f:
        f.write("\n".join(po_lines))

    merged_path = "/tmp/merged_de.po"
    # Merge with msgcat --use-first so curated translations override
    subprocess.run(["msgcat", "--use-first", curated_path, DE_PO_PATH, "-o", merged_path], check=True)

    # Merge against POT template + upstream compendium
    cmd = ["msgmerge", "--update", merged_path, POT_PATH]
    if os.path.exists(COMPENDIUM):
        cmd = ["msgmerge", "-C", COMPENDIUM, "--update", merged_path, POT_PATH]
    subprocess.run(cmd, check=True)

    # Overwrite DE_PO_PATH
    subprocess.run(["cp", merged_path, DE_PO_PATH], check=True)

    # Compile MO
    os.makedirs(os.path.dirname(LOCALEDIR_MO), exist_ok=True)
    subprocess.run(["msgfmt", "-o", LOCALEDIR_MO, DE_PO_PATH], check=True)
    subprocess.run(["msgfmt", "--statistics", DE_PO_PATH], check=True)
    print("de.po synchronized and MO compiled successfully.")

if __name__ == "__main__":
    main()
