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
    PrimaryZoom,
    PrimaryActions,
    PrimaryApp,
    FileNew,
    File,
    Edit,
    ViewSwitch,
    View,
    ViewLayout,
    ViewColumns,
    ViewThreads,
    ViewDensity,
    ViewZoom,
    ViewDetails,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ShortcutSection {
    Mail,
    Composer,
    Calendar,
    ContactsTasks,
    General,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ShortcutGroup {
    // Mail
    MailNavigation,
    MailMessage,
    MailOrganize,
    MailView,
    // Composer
    ComposerSend,
    ComposerEditing,
    // Calendar
    CalendarEvents,
    // Contacts & Tasks
    Contacts,
    TasksMemos,
    // General
    GeneralModules,
    GeneralApp,
    GeneralPanes,
    // Backward compatibility variants
    General,
    Mail,
    Calendar,
    Modules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShortcutSpec {
    pub section: ShortcutSection,
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
        "win.new-task", "win.oof-settings", "win.redo", "win.search",
        "win.show-help-overlay", "win.switch-calendar", "win.switch-contacts",
        "win.switch-mail", "win.switch-tasks", "win.undo",
        "win.filter-editor", "win.search-folder-editor", "win.mail-receive-all",
        "win.mail-send-all", "win.mail-send-receive", "win.mail-stop",
        "win.preferences", "win.toggle-top-switcher",
        "win.toggle-toolbar", "win.toggle-statusbar", "win.toggle-debug-console",
        "win.work-offline", "win.work-online",
    ],
    ActiveMail => [
        "win.apply-filters", "win.mail-density-compact",
        "win.mail-density-normal", "win.mail-density-spacious",
        "win.new-folder", "win.refresh-mail",
        "win.search-advanced",
        "win.toggle-all-headers", "win.zoom-in", "win.zoom-out", "win.zoom-reset",
        "win.toggle-mail-status", "win.toggle-mail-flagged",
        "win.toggle-mail-attachment", "win.toggle-mail-avatar", "win.toggle-mail-date",
        "win.toggle-mail-labels", "win.toggle-mail-preview-text",
        "win.toggle-mail-recipient", "win.toggle-mail-sender",
        "win.toggle-mail-show-deleted", "win.toggle-mail-statusbar",
        "win.toggle-mail-threading", "win.toggle-mail-toolbar", "win.toggle-preview",
        "win.toggle-sidebar", "win.toggle-todo-bar", "win.use-classic-mail-layout", "win.use-modern-mail-layout",
        "win.use-vertical-mail-layout", "win.use-horizontal-mail-layout",
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
        "win.toggle-flag", "win.toggle-important", "win.toggle-mail-light-mode", "win.undelete",
        "win.view-message-source",
    ],
    FolderSelection => [
        "win.delete-folder", "win.empty-junk", "win.empty-trash", "win.expunge",
        "win.folder-edit-sort-order", "win.folder-properties", "win.folder-subscriptions",
        "win.mark-all-read", "win.refresh-folder", "win.rename-folder",
    ],
    ActiveCalendar => ["win.purge-calendar", "win.export-calendar"],
};

macro_rules! menu_specs {
    ($($path:ident => [$(($label:literal, $action:literal)),+ $(,)?]),+ $(,)?) => {
        &[$($(MenuActionSpec { path: MenuPath::$path, label: $label, action: $action },)+)+]
    };
}

pub(crate) const PRIMARY_MENU_ACTIONS: &[MenuActionSpec] = menu_specs! {
    PrimaryZoom => [
        ("Zoom In", "win.zoom-in"),
        ("Normal Size", "win.zoom-reset"),
        ("Zoom Out", "win.zoom-out"),
        ("Show Action Toolbar", "win.toggle-toolbar"),
    ],
    PrimaryActions => [
        ("Send & Receive", "win.mail-send-receive"),
    ],
    PrimaryApp => [
        ("Preferences", "app.preferences"),
        ("Keyboard Shortcuts", "win.show-help-overlay"),
        ("About Meridian", "app.about"),
    ],
    FileNew => [
        ("New Email", "app.compose"), ("New Contact", "win.new-contact"),
        ("New Event", "win.new-event"), ("New Task", "win.new-task"),
        ("New Memo", "win.new-memo"), ("New Folder...", "win.new-folder"),
    ],
    File => [
        ("Local Drafts", "app.drafts"),
        ("Send & Receive", "win.mail-send-receive"),
        ("Work Offline", "win.work-offline"), ("Work Online", "win.work-online"),
        ("Import Data…", "win.import"),
        ("Export Selected Email", "win.export"),
        ("Export Calendar…", "win.export-calendar"),
        ("Print Selected Email…", "win.mail-print"),
        ("Print Preview of Selected Email…", "win.mail-print-preview"),
    ],
    Edit => [
        ("Undo", "win.undo"), ("Redo", "win.redo"),
        ("Undelete", "win.undelete"), ("Search Email", "win.search"),
        ("Advanced Search…", "win.search-advanced"),
        ("Find in Message…", "win.find-in-message"),
        ("Global Search", "app.global-search"),
        ("Purge Old Events…", "win.purge-calendar"),
        ("Edit Filter Rules...", "win.filter-editor"),
        ("Manage Search Folders...", "win.search-folder-editor"),
        ("Preferences", "app.preferences"),
    ],
    ViewSwitch => [
        ("Mail", "win.switch-mail"), ("Calendar", "win.switch-calendar"),
        ("Contacts", "win.switch-contacts"), ("Tasks", "win.switch-tasks"),
    ],
    View => [
        ("Show/Hide Folder Sidebar", "win.toggle-sidebar"),
        ("Show/Hide Preview Window", "win.toggle-preview"),
    ],
    ViewLayout => [
        ("Vertical Mail Layout (3 Columns)", "win.use-vertical-mail-layout"),
        ("Horizontal Mail Layout (Preview Below)", "win.use-horizontal-mail-layout"),
        ("Classic Mail Layout", "win.use-classic-mail-layout"),
        ("Show/Hide To Do Bar for Today", "win.toggle-todo-bar"),
        ("Show/Hide Toolbar", "win.toggle-toolbar"),
        ("Module Buttons in Headerbar", "win.toggle-top-switcher"),
        ("Show/Hide Statusbar", "win.toggle-statusbar"),
    ],
    ViewColumns => [
        ("Show/Hide Status", "win.toggle-mail-status"),
        ("Show/Hide Flag", "win.toggle-mail-flagged"),
        ("Show/Hide Avatars", "win.toggle-mail-avatar"),
        ("Show/Hide Senders", "win.toggle-mail-sender"),
        ("Show/Hide Recipients", "win.toggle-mail-recipient"),
        ("Show/Hide Dates", "win.toggle-mail-date"),
        ("Show/Hide Attachment Icons", "win.toggle-mail-attachment"),
        ("Show/Hide Preview Text", "win.toggle-mail-preview-text"),
        ("Show/Hide Labels", "win.toggle-mail-labels"),
    ],
    ViewThreads => [
        ("Group/Ungroup Conversations", "win.toggle-mail-threading"),
        ("Expand All Conversations", "win.expand-all-threads"),
        ("Collapse All Conversations", "win.collapse-all-threads"),
    ],
    ViewDensity => [
        ("Compact", "win.mail-density-compact"),
        ("Standard", "win.mail-density-normal"),
        ("Spacious", "win.mail-density-spacious"),
    ],
    ViewZoom => [
        ("Zoom In", "win.zoom-in"),
        ("Zoom Out", "win.zoom-out"),
        ("Normal Size", "win.zoom-reset"),
    ],
    ViewDetails => [
        ("Show All Headers", "win.toggle-all-headers"),
        ("Show Message Source", "win.view-message-source"),
        ("Show Deleted Messages", "win.toggle-mail-show-deleted"),
        ("Show/Hide Developer Console", "win.toggle-debug-console"),
    ],
    MessageGoto => [
        ("Next Message", "win.mail-next"),
        ("Next Unread Message", "win.mail-next-unread"),
        ("Previous Message", "win.mail-prev"),
        ("Previous Unread Message", "win.mail-prev-unread"),
        ("Containing Folder", "win.goto-containing-folder"),
    ],
    MessageForward => [
        ("Forward", "win.forward"), ("Forward Inline", "win.forward-inline"),
        ("Forward Quoted", "win.forward-quoted"),
        ("Forward as Attachment", "win.forward-attached"),
    ],
    MessageBeforeForward => [
        ("Reply", "win.reply"), ("Reply All", "win.reply-all"),
        ("Reply to List", "win.reply-list"),
        ("Reply Alternative…", "win.reply-alternative"),
    ],
    MessageAfterForward => [
        ("Save as EML...", "win.export"), ("Print…", "win.mail-print"),
        ("Print Preview…", "win.mail-print-preview"),
        ("Copy Message Link", "win.copy-message-link"),
        ("Add Sender to Address Book", "win.add-sender-contact"),
        ("Edit as New Message…", "win.edit-as-new"),
        ("Select All Messages", "win.select-all-mail"),
        ("Mark as Read", "win.mark-read"),
        ("Mark as Unread", "win.mark-unread"),
        ("Mark Conversation as Read", "win.mark-thread-read"),
        ("Delete Conversation", "win.delete-thread"),
        ("Toggle Flag", "win.toggle-flag"),
        ("Toggle Important", "win.toggle-important"),
        ("Mark for Follow Up…", "win.mail-follow-up"),
        ("Edit Labels...", "win.edit-labels"),
        ("Move to Folder...", "win.move-message"),
        ("Copy to Folder...", "win.copy-message"), ("Archive", "win.archive"),
        ("Mark as Junk", "win.mark-junk"),
        ("Mark as Not Junk", "win.mark-not-junk"), ("Delete", "win.delete"),
        ("Undelete", "win.undelete"),
    ],
    Folder => [
        ("New Folder...", "win.new-folder"), ("Rename Folder...", "win.rename-folder"),
        ("Delete Folder...", "win.delete-folder"),
        ("Refresh Folder", "win.refresh-folder"),
        ("Mark All as Read", "win.mark-all-read"),
        ("Next Unread Folder", "win.next-unread-folder"),
        ("Refresh Mail", "win.refresh-mail"), ("Empty Trash", "win.empty-trash"),
        ("Empty Junk", "win.empty-junk"),
        ("Expunge", "win.expunge"),
        ("Manage Subscriptions...", "win.folder-subscriptions"),
        ("Edit Sort Order...", "win.folder-edit-sort-order"),
        ("Folder Properties...", "win.folder-properties"),
    ],
    Tools => [
        ("Out of Office (OOF)...", "win.oof-settings"),
        ("Edit Filter Rules...", "win.filter-editor"),
        ("Manage Search Folders...", "win.search-folder-editor"),
        ("Account Settings...", "win.account-setup"),
    ],
    Help => [
        ("Developer Console", "win.toggle-debug-console"),
        ("Keyboard Shortcuts", "win.show-help-overlay"),
        ("About Meridian", "app.about"),
    ],
    Transport => [
        ("Send & Receive", "win.mail-send-receive"),
        ("Receive All Accounts", "win.mail-receive-all"),
        ("Send Pending Messages", "win.mail-send-all"),
        ("Cancel", "win.mail-stop"),
    ],
};

pub(crate) const SHORTCUTS: &[ShortcutSpec] = &[
    // ── Mail: Navigation & Search ──
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Next Message",
        action: "win.mail-next",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Previous Message",
        action: "win.mail-prev",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Next Unread Message",
        action: "win.mail-next-unread",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Previous Unread Message",
        action: "win.mail-prev-unread",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Next Unread Folder",
        action: "win.next-unread-folder",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Focus Search",
        action: "win.search",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailNavigation,
        title: "Send & Receive All",
        action: "win.mail-send-receive",
    },

    // ── Mail: Message Actions ──
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Compose New Message",
        action: "win.new",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Reply",
        action: "win.reply",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Reply All",
        action: "win.reply-all",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Forward",
        action: "win.forward",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Archive Message",
        action: "win.archive",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Delete Message",
        action: "win.delete",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Undelete Message",
        action: "win.undelete",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Move to Folder",
        action: "win.move-message",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Copy to Folder",
        action: "win.copy-message",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Refresh Mail",
        action: "win.refresh-mail",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Apply Filters",
        action: "win.apply-filters",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Export Selected Email",
        action: "win.export",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Print Message",
        action: "win.mail-print",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "Find in Message",
        action: "win.find-in-message",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailMessage,
        title: "View Message Source",
        action: "win.view-message-source",
    },

    // ── Mail: Organize & Flags ──
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark as Read",
        action: "win.mark-read",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark as Unread",
        action: "win.mark-unread",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark All as Read",
        action: "win.mark-all-read",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark Conversation as Read",
        action: "win.mark-thread-read",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Toggle Flag",
        action: "win.toggle-flag",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Toggle Important",
        action: "win.toggle-important",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark as Junk",
        action: "win.mark-junk",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Mark as Not Junk",
        action: "win.mark-not-junk",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailOrganize,
        title: "Select All Messages",
        action: "win.select-all-mail",
    },

    // ── Mail: View & Zoom ──
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Toggle Preview Window",
        action: "win.toggle-preview",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Group by Thread",
        action: "win.toggle-mail-threading",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Expand All Conversations",
        action: "win.expand-all-threads",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Collapse All Conversations",
        action: "win.collapse-all-threads",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Zoom In",
        action: "win.zoom-in",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Zoom Out",
        action: "win.zoom-out",
    },
    ShortcutSpec {
        section: ShortcutSection::Mail,
        group: ShortcutGroup::MailView,
        title: "Normal Size",
        action: "win.zoom-reset",
    },

    // ── Calendar ──
    ShortcutSpec {
        section: ShortcutSection::Calendar,
        group: ShortcutGroup::CalendarEvents,
        title: "Purge Old Events",
        action: "win.purge-calendar",
    },
    ShortcutSpec {
        section: ShortcutSection::Calendar,
        group: ShortcutGroup::CalendarEvents,
        title: "Switch to Calendar",
        action: "win.switch-calendar",
    },

    // ── Contacts & Tasks ──
    ShortcutSpec {
        section: ShortcutSection::ContactsTasks,
        group: ShortcutGroup::Contacts,
        title: "Switch to Contacts",
        action: "win.switch-contacts",
    },
    ShortcutSpec {
        section: ShortcutSection::ContactsTasks,
        group: ShortcutGroup::TasksMemos,
        title: "Switch to Tasks",
        action: "win.switch-tasks",
    },

    // ── General: Modules ──
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralModules,
        title: "Mail",
        action: "win.switch-mail",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralModules,
        title: "Calendar",
        action: "win.switch-calendar",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralModules,
        title: "Contacts",
        action: "win.switch-contacts",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralModules,
        title: "Tasks",
        action: "win.switch-tasks",
    },

    // ── General: Application ──
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Global Search",
        action: "app.global-search",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Open Preferences",
        action: "app.preferences",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Show Keyboard Shortcuts",
        action: "win.show-help-overlay",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Undo",
        action: "win.undo",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Redo",
        action: "win.redo",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralApp,
        title: "Quit",
        action: "app.quit",
    },

    // ── General: Panes & Views ──
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralPanes,
        title: "Toggle Sidebar",
        action: "win.toggle-sidebar",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralPanes,
        title: "Toggle Statusbar",
        action: "win.toggle-statusbar",
    },
    ShortcutSpec {
        section: ShortcutSection::General,
        group: ShortcutGroup::GeneralPanes,
        title: "Toggle Developer Console",
        action: "win.toggle-debug-console",
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
            item.action == "win.search-advanced" && item.label == "Advanced Search…"
        }));
    }

    #[test]
    fn view_menu_is_compact_and_structured_without_overflow() {
        // The top-level View path should contain only primary items, not a flat 26-item wall
        let top_level_count = menu_actions(MenuPath::View).count();
        assert!(
            top_level_count <= 4,
            "Top-level View menu is too large ({top_level_count} items); should use submenus"
        );

        // Submenus must partition the remaining view actions
        assert!(menu_actions(MenuPath::ViewLayout).any(|i| i.action == "win.use-classic-mail-layout"));
        assert!(menu_actions(MenuPath::ViewColumns).any(|i| i.action == "win.toggle-mail-sender"));
        assert!(menu_actions(MenuPath::ViewThreads).any(|i| i.action == "win.toggle-mail-threading"));
        assert!(menu_actions(MenuPath::ViewDensity).any(|i| i.action == "win.mail-density-compact"));
        assert!(menu_actions(MenuPath::ViewZoom).any(|i| i.action == "win.zoom-in"));
        assert!(menu_actions(MenuPath::ViewDetails).any(|i| i.action == "win.toggle-all-headers"));
    }
}
