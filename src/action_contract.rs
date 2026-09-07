// SPDX-License-Identifier: GPL-3.0-or-later
//
// Shared contract for user-visible shell actions, menus, and keyboard help.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ActionScope {
    Always,
    ActiveMail,
    MailList,
    MessageSelection,
    FolderSelection,
    ActiveCalendar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionSpec {
    pub action: &'static str,
    pub scope: ActionScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MenuPath {
    FileNew,
    File,
    Edit,
    ViewSwitch,
    View,
    ViewDensity,
    ViewAfterDensity,
    MessageGoto,
    MessageForward,
    MessageBeforeForward,
    MessageAfterForward,
    Folder,
    Tools,
    Help,
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MenuActionSpec {
    pub path: MenuPath,
    pub label: &'static str,
    pub action: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShortcutGroup {
    General,
    Mail,
    Modules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShortcutSpec {
    pub group: ShortcutGroup,
    pub title: &'static str,
    pub action: &'static str,
}

macro_rules! action_specs {
    ($($scope:ident => [$($action:literal),+ $(,)?]),+ $(,)?) => {
        &[$($(ActionSpec { action: $action, scope: ActionScope::$scope },)+)+]
    };
}

/// Actions exposed through the daily shell UI. Internal reminder/deep-link
/// actions are intentionally outside this menu-and-shortcut contract.
pub(crate) const ACTIONS: &[ActionSpec] = action_specs! {
    Always => [
        "app.about", "app.compose", "app.drafts", "app.global-search",
        "app.preferences", "app.quit", "win.account-setup", "win.import",
        "win.new", "win.new-contact", "win.new-event", "win.new-memo",
        "win.new-task", "win.oof-settings", "win.search",
        "win.show-help-overlay", "win.switch-calendar", "win.switch-contacts",
        "win.switch-mail", "win.switch-memos", "win.switch-tasks",
        "win.filter-editor", "win.search-folder-editor", "win.mail-receive-all",
        "win.mail-send-all", "win.mail-send-receive", "win.mail-stop",
        "win.preferences", "win.toggle-top-switcher",
        "win.work-offline", "win.work-online",
    ],
    ActiveMail => [
        "win.apply-filters", "win.mail-density-compact",
        "win.mail-density-normal", "win.mail-density-spacious",
        "win.new-folder", "win.redo", "win.refresh-mail", "win.undo",
        "win.search-advanced",
        "win.toggle-all-headers", "win.zoom-in", "win.zoom-out", "win.zoom-reset",
        "win.toggle-mail-attachment", "win.toggle-mail-avatar", "win.toggle-mail-date",
        "win.toggle-mail-labels", "win.toggle-mail-preview-text",
        "win.toggle-mail-recipient", "win.toggle-mail-sender",
        "win.toggle-mail-show-deleted", "win.toggle-mail-statusbar",
        "win.toggle-mail-threading", "win.toggle-mail-toolbar", "win.toggle-preview",
        "win.toggle-sidebar", "win.use-classic-mail-layout", "win.use-modern-mail-layout",
    ],
    MailList => [
        "win.collapse-all-threads", "win.expand-all-threads", "win.mail-next",
        "win.mail-next-unread", "win.mail-prev", "win.mail-prev-unread",
        "win.next-unread-folder", "win.select-all-mail",
    ],
    MessageSelection => [
        "win.add-sender-contact", "win.archive", "win.copy-message",
        "win.copy-message-link", "win.delete", "win.delete-thread", "win.edit-as-new",
        "win.edit-labels", "win.export", "win.find-in-message", "win.forward",
        "win.forward-attached", "win.forward-inline", "win.forward-quoted",
        "win.goto-containing-folder", "win.mail-follow-up", "win.mail-print",
        "win.mail-print-preview", "win.mark-junk", "win.mark-not-junk", "win.mark-read",
        "win.mark-thread-read", "win.mark-unread", "win.move-message", "win.reply",
        "win.reply-all", "win.reply-alternative", "win.reply-list",
        "win.toggle-flag", "win.toggle-important", "win.undelete",
        "win.view-message-source",
    ],
    FolderSelection => [
        "win.delete-folder", "win.empty-junk", "win.empty-trash", "win.expunge",
        "win.folder-edit-sort-order", "win.folder-properties", "win.folder-subscriptions",
        "win.mark-all-read", "win.refresh-folder", "win.rename-folder",
    ],
    ActiveCalendar => ["win.purge-calendar"],
};

macro_rules! menu_specs {
    ($($path:ident => [$(($label:literal, $action:literal)),+ $(,)?]),+ $(,)?) => {
        &[$($(MenuActionSpec { path: MenuPath::$path, label: $label, action: $action },)+)+]
    };
}

pub(crate) const PRIMARY_MENU_ACTIONS: &[MenuActionSpec] = menu_specs! {
    FileNew => [
        ("Neue E-Mail", "app.compose"), ("Neuer Kontakt", "win.new-contact"),
        ("Neuer Termin", "win.new-event"), ("Neue Aufgabe", "win.new-task"),
        ("Neue Notiz", "win.new-memo"), ("Neuer Ordner...", "win.new-folder"),
    ],
    File => [
        ("Lokale Entwürfe", "app.drafts"),
        ("Senden & Empfangen", "win.mail-send-receive"),
        ("Offline arbeiten", "win.work-offline"), ("Online arbeiten", "win.work-online"),
        ("Daten importieren…", "win.import"),
        ("Ausgewählte E-Mail exportieren", "win.export"),
        ("Ausgewählte E-Mail drucken…", "win.mail-print"),
        ("Druckvorschau der ausgewählten E-Mail…", "win.mail-print-preview"),
    ],
    Edit => [
        ("Rückgängig", "win.undo"), ("Wiederholen", "win.redo"),
        ("Löschen rückgängig machen", "win.undelete"), ("E-Mail suchen", "win.search"),
        ("Erweiterte Suche…", "win.search-advanced"),
        ("In angezeigter Nachricht suchen…", "win.find-in-message"),
        ("Globale Suche", "app.global-search"),
        ("Vergangene Termine löschen…", "win.purge-calendar"),
        ("Filterregeln bearbeiten...", "win.filter-editor"),
        ("Suchordner verwalten...", "win.search-folder-editor"),
        ("Einstellungen", "app.preferences"),
    ],
    ViewSwitch => [
        ("Mail", "win.switch-mail"), ("Kalender", "win.switch-calendar"),
        ("Kontakte", "win.switch-contacts"), ("Aufgaben", "win.switch-tasks"),
        ("Notizen", "win.switch-memos"),
    ],
    View => [
        ("Modul-Schaltflächen in Kopfleiste", "win.toggle-top-switcher"),
        ("Ordner-Seitenleiste ein-/ausblenden", "win.toggle-sidebar"),
        ("Vorschaufenster ein-/ausblenden", "win.toggle-preview"),
        ("Alle Nachrichtenkopfzeilen anzeigen", "win.toggle-all-headers"),
        ("Nachrichtenquelle anzeigen", "win.view-message-source"),
        ("Klassische Evolution-Mail-Anordnung", "win.use-classic-mail-layout"),
        ("Moderne Mail-Anordnung", "win.use-modern-mail-layout"),
        ("Klassische Mail-Werkzeugleiste ein-/ausblenden", "win.toggle-mail-toolbar"),
        ("Statusinformationen unten ein-/ausblenden", "win.toggle-mail-statusbar"),
        ("Unterhaltungen gruppieren/auflösen", "win.toggle-mail-threading"),
        ("Gelöschte Nachrichten anzeigen", "win.toggle-mail-show-deleted"),
        ("Vorschautext in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-preview-text"),
        ("Avatare in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-avatar"),
        ("Absender in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-sender"),
        ("Empfänger in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-recipient"),
        ("Datum in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-date"),
        ("Anhangssymbole in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-attachment"),
        ("Labels in Nachrichtenzeilen ein-/ausblenden", "win.toggle-mail-labels"),
    ],
    ViewAfterDensity => [
        ("Schrift vergrößern", "win.zoom-in"), ("Schrift verkleinern", "win.zoom-out"),
        ("Normale Schriftgröße", "win.zoom-reset"),
        ("Alle Unterhaltungen aufklappen", "win.expand-all-threads"),
        ("Alle Unterhaltungen einklappen", "win.collapse-all-threads"),
    ],
    ViewDensity => [
        ("Kompakt", "win.mail-density-compact"),
        ("Standard", "win.mail-density-normal"),
        ("Geräumig", "win.mail-density-spacious"),
    ],
    MessageGoto => [
        ("Nächste Nachricht", "win.mail-next"),
        ("Nächste ungelesene Nachricht", "win.mail-next-unread"),
        ("Vorherige Nachricht", "win.mail-prev"),
        ("Vorherige ungelesene Nachricht", "win.mail-prev-unread"),
        ("Enthaltender Ordner", "win.goto-containing-folder"),
    ],
    MessageForward => [
        ("Weiterleiten", "win.forward"), ("Inline weiterleiten", "win.forward-inline"),
        ("Zitiert weiterleiten", "win.forward-quoted"),
        ("Als Anhang weiterleiten", "win.forward-attached"),
    ],
    MessageBeforeForward => [
        ("Antworten", "win.reply"), ("Allen antworten", "win.reply-all"),
        ("An Liste antworten", "win.reply-list"),
        ("Alternativ antworten…", "win.reply-alternative"),
    ],
    MessageAfterForward => [
        ("Als EML speichern...", "win.export"), ("Drucken…", "win.mail-print"),
        ("Druckvorschau…", "win.mail-print-preview"),
        ("Nachrichtenlink kopieren", "win.copy-message-link"),
        ("Absender zum Adressbuch hinzufügen", "win.add-sender-contact"),
        ("Als neue Nachricht bearbeiten…", "win.edit-as-new"),
        ("Alle sichtbaren Nachrichten auswählen", "win.select-all-mail"),
        ("Als gelesen markieren", "win.mark-read"),
        ("Als ungelesen markieren", "win.mark-unread"),
        ("Unterhaltung als gelesen markieren", "win.mark-thread-read"),
        ("Unterhaltung löschen", "win.delete-thread"),
        ("Markierung umschalten", "win.toggle-flag"),
        ("Wichtigkeitsmarkierung umschalten", "win.toggle-important"),
        ("Zur Nachverfolgung markieren…", "win.mail-follow-up"),
        ("Labels bearbeiten...", "win.edit-labels"),
        ("In Ordner verschieben...", "win.move-message"),
        ("In Ordner kopieren...", "win.copy-message"), ("Archivieren", "win.archive"),
        ("Als Spam markieren", "win.mark-junk"),
        ("Als Nicht-Spam markieren", "win.mark-not-junk"), ("Löschen", "win.delete"),
        ("Löschen rückgängig machen", "win.undelete"),
    ],
    Folder => [
        ("Neuer Ordner...", "win.new-folder"), ("Ordner umbenennen...", "win.rename-folder"),
        ("Ordner löschen...", "win.delete-folder"),
        ("Ordner aktualisieren", "win.refresh-folder"),
        ("Alle als gelesen markieren", "win.mark-all-read"),
        ("Nächster ungelesener Ordner", "win.next-unread-folder"),
        ("Ordner aktualisieren", "win.refresh-mail"), ("Papierkorb leeren", "win.empty-trash"),
        ("Spam-Ordner leeren", "win.empty-junk"),
        ("Endgültig löschen (Expunge)", "win.expunge"),
        ("Abonnements verwalten...", "win.folder-subscriptions"),
        ("Sortierreihenfolge bearbeiten...", "win.folder-edit-sort-order"),
        ("Eigenschaften des Ordners...", "win.folder-properties"),
    ],
    Tools => [
        ("Abwesenheitsnotiz (OOF)...", "win.oof-settings"),
        ("Filterregeln bearbeiten...", "win.filter-editor"),
        ("Suchordner verwalten...", "win.search-folder-editor"),
        ("Kontoeinstellungen...", "win.account-setup"),
    ],
    Help => [
        ("Tastenkombinationen", "win.show-help-overlay"),
        ("Über Evolution", "app.about"),
    ],
    Transport => [
        ("Senden & Empfangen", "win.mail-send-receive"),
        ("Alle Konten empfangen", "win.mail-receive-all"),
        ("Ausstehende Nachrichten senden", "win.mail-send-all"),
        ("Abbrechen", "win.mail-stop"),
    ],
};

pub(crate) const SHORTCUTS: &[ShortcutSpec] = &[
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Einstellungen öffnen",
        action: "app.preferences",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Tastaturkürzel anzeigen",
        action: "win.show-help-overlay",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Anwendungsmenü / Beenden",
        action: "app.quit",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Mail aktualisieren",
        action: "win.refresh-mail",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Sidebar ein-/ausblenden",
        action: "win.toggle-sidebar",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Suche im aktiven Modul fokussieren",
        action: "win.search",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Globale lokale Suche",
        action: "app.global-search",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Rückgängig",
        action: "win.undo",
    },
    ShortcutSpec {
        group: ShortcutGroup::General,
        title: "Wiederholen",
        action: "win.redo",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Neue Nachricht verfassen",
        action: "win.new",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Antworten",
        action: "win.reply",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Allen antworten",
        action: "win.reply-all",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Weiterleiten",
        action: "win.forward",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nachricht löschen",
        action: "win.delete",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Alle Nachrichten auswählen",
        action: "win.select-all-mail",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Wichtigkeit umschalten",
        action: "win.toggle-important",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Gelesen markieren",
        action: "win.mark-read",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Ungelesen markieren",
        action: "win.mark-unread",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nachricht verschieben",
        action: "win.move-message",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nachricht kopieren",
        action: "win.copy-message",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Quelltext anzeigen",
        action: "win.view-message-source",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Filter anwenden",
        action: "win.apply-filters",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nächste Nachricht",
        action: "win.mail-next",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Vorherige Nachricht",
        action: "win.mail-prev",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nächste ungelesene Nachricht",
        action: "win.mail-next-unread",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Vorherige ungelesene Nachricht",
        action: "win.mail-prev-unread",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Vorschaufenster ein-/ausblenden",
        action: "win.toggle-preview",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Unterhaltungen gruppieren/auflösen",
        action: "win.toggle-mail-threading",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Angezeigte Nachricht durchsuchen",
        action: "win.find-in-message",
    },
    ShortcutSpec {
        group: ShortcutGroup::Mail,
        title: "Nachricht drucken",
        action: "win.mail-print",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Mail",
        action: "win.switch-mail",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Kalender",
        action: "win.switch-calendar",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Kontakte",
        action: "win.switch-contacts",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Aufgaben",
        action: "win.switch-tasks",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Notizen",
        action: "win.switch-memos",
    },
    ShortcutSpec {
        group: ShortcutGroup::Modules,
        title: "Alte Termine bereinigen (nur im Kalender)",
        action: "win.purge-calendar",
    },
];

pub(crate) fn menu_actions(path: MenuPath) -> impl Iterator<Item = &'static MenuActionSpec> {
    PRIMARY_MENU_ACTIONS
        .iter()
        .filter(move |item| item.path == path)
}

#[cfg(test)]
pub(crate) fn action_scope(action: &str) -> Option<ActionScope> {
    ACTIONS
        .iter()
        .find(|spec| spec.action == action)
        .map(|spec| spec.scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn exported_action_ids_are_unique() {
        let mut ids = HashSet::new();
        for spec in ACTIONS {
            assert!(
                ids.insert(spec.action),
                "duplicate action id: {}",
                spec.action
            );
        }
    }

    #[test]
    fn menu_and_shortcut_actions_are_registered() {
        for action in PRIMARY_MENU_ACTIONS
            .iter()
            .map(|item| item.action)
            .chain(SHORTCUTS.iter().map(|item| item.action))
        {
            assert!(action_scope(action).is_some(), "unknown action: {action}");
        }
    }

    #[test]
    fn menu_has_no_duplicate_placement() {
        let mut placements = HashSet::new();
        for item in PRIMARY_MENU_ACTIONS {
            assert!(
                placements.insert((item.path, item.action)),
                "duplicate menu placement: {:?}/{}",
                item.path,
                item.action
            );
        }
    }

    #[test]
    fn selection_scopes_do_not_capture_shell_actions() {
        for action in [
            "app.preferences",
            "app.quit",
            "win.search",
            "win.switch-mail",
            "win.switch-calendar",
        ] {
            assert_eq!(action_scope(action), Some(ActionScope::Always));
        }
        assert_eq!(
            action_scope("win.reply"),
            Some(ActionScope::MessageSelection)
        );
        assert_eq!(
            action_scope("win.delete-folder"),
            Some(ActionScope::FolderSelection)
        );
        assert_eq!(
            action_scope("win.purge-calendar"),
            Some(ActionScope::ActiveCalendar)
        );
    }

    #[test]
    fn advanced_search_is_mail_scoped_and_visible_next_to_mail_search() {
        assert_eq!(
            action_scope("win.search-advanced"),
            Some(ActionScope::ActiveMail)
        );
        assert!(menu_actions(MenuPath::Edit).any(|item| {
            item.action == "win.search-advanced" && item.label == "Erweiterte Suche…"
        }));
    }
}
