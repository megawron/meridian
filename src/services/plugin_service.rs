// SPDX-License-Identifier: GPL-3.0-or-later
//
// plugin_service — Evolution plugin registry, lifecycle, and runtime hooks.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCategory {
    Mail,
    Security,
    Junk,
    Utilities,
    Calendar,
}

impl PluginCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "E-Mail &amp; Verfassen",
            Self::Security => "Sicherheit &amp; Krypto",
            Self::Junk => "Junk &amp; Spamfilter",
            Self::Utilities => "Hilfsprogramme &amp; Editoren",
            Self::Calendar => "Kalender &amp; iTIP",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Mail => "mail-send-symbolic",
            Self::Security => "channel-secure-symbolic",
            Self::Junk => "mail-mark-junk-symbolic",
            Self::Utilities => "applications-utilities-symbolic",
            Self::Calendar => "x-office-calendar-symbolic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: PluginCategory,
    pub author: &'static str,
    pub version: &'static str,
    pub is_configurable: bool,
}

pub const STANDARD_PLUGINS: &[PluginDescriptor] = &[
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.attachment-reminder",
        name: "Anhang-Erinnerung",
        description: "Überprüft E-Mails vor dem Absenden auf vergessene Anhänge anhand von Schlüsselwörtern wie „Anhang“, „angehängt“, „attached“ etc.",
        category: PluginCategory::Mail,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: true,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.mail-notification",
        name: "E-Mail-Benachrichtigungen",
        description: "Zeigt Desktop-Benachrichtigungen, Status-Icons und optionale Klänge bei neu eingehenden Nachrichten an.",
        category: PluginCategory::Mail,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: true,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.spamassassin",
        name: "SpamAssassin Junk-Filter",
        description: "Filtert unerwünschte Werbemails mit dem lokalen SpamAssassin-Daemon (spamd) oder der SpamAssassin-Headeranalyse (X-Spam-Status).",
        category: PluginCategory::Junk,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: true,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.bogofilter",
        name: "Bogofilter Junk-Filter",
        description: "Bayesscher Spamfilter zur präzisen Klassifizierung unerwünschter E-Mails mittels Bogofilter und Headerprüfung (X-Bogosity).",
        category: PluginCategory::Junk,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: true,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.itip-formatter",
        name: "vCalendar / iTIP Einladungs-Formatierer",
        description: "Erkennt Kalendereinladungen in Nachrichten und erlaubt die direkte Zusage, Absage oder Aktualisierung von Terminen mit einem Klick.",
        category: PluginCategory::Calendar,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: false,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.external-editor",
        name: "Externer Texteditor",
        description: "Ermöglicht das Verfassen von E-Mails in einem frei wählbaren externen Editor (z.B. GNOME Text Editor, Gedit, Vim, VS Code).",
        category: PluginCategory::Utilities,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: true,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.templates",
        name: "E-Mail-Vorlagen &amp; Textbausteine",
        description: "Verwaltet wiederverwendbare Vorlagen und Textbausteine für schnelles Antworten und standardisierte Anschreiben.",
        category: PluginCategory::Mail,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: false,
    },
    PluginDescriptor {
        id: "org.gnome.evolution.plugin.face",
        name: "Face / Gravatar Header-Icons",
        description: "Liest X-Face-, Face- und Gravatar-Avatar-Header aus und stellt sie im Nachrichtenkopf dar.",
        category: PluginCategory::Mail,
        author: "Evolution Team",
        version: "4.0.0",
        is_configurable: false,
    },
];

pub struct PluginManager;

impl PluginManager {
    pub fn all_plugins() -> &'static [PluginDescriptor] {
        STANDARD_PLUGINS
    }

    pub fn find_plugin(id: &str) -> Option<&'static PluginDescriptor> {
        STANDARD_PLUGINS.iter().find(|p| p.id == id)
    }

    pub fn is_plugin_enabled(id: &str, disabled_plugins: &[String]) -> bool {
        !disabled_plugins.iter().any(|d| d == id)
    }

    pub fn enable_plugin(id: &str, disabled_plugins: &mut Vec<String>) -> bool {
        let initial_len = disabled_plugins.len();
        disabled_plugins.retain(|d| d != id);
        disabled_plugins.len() < initial_len
    }

    pub fn disable_plugin(id: &str, disabled_plugins: &mut Vec<String>) -> bool {
        if !disabled_plugins.iter().any(|d| d == id) {
            disabled_plugins.push(id.to_string());
            true
        } else {
            false
        }
    }

    pub fn toggle_plugin(id: &str, disabled_plugins: &mut Vec<String>) -> bool {
        if disabled_plugins.iter().any(|d| d == id) {
            Self::enable_plugin(id, disabled_plugins);
            true // now enabled
        } else {
            Self::disable_plugin(id, disabled_plugins);
            false // now disabled
        }
    }

    /// Runtime hook: check if message body mentions an attachment while no attachment is attached.
    pub fn check_missing_attachment(
        body_text: &str,
        has_attachments: bool,
        reminder_enabled: bool,
        keywords: &[String],
    ) -> Option<String> {
        if !reminder_enabled || has_attachments {
            return None;
        }

        let lower_body = body_text.to_lowercase();
        let default_keywords = ["anhang", "angehängt", "attached", "attachment", "enclosed", "anbei", "anlage"];

        let matched_keyword = if keywords.is_empty() {
            default_keywords.iter().find(|&&kw| lower_body.contains(kw)).map(|&s| s.to_string())
        } else {
            keywords.iter().find(|kw| {
                let kw_clean = kw.trim().to_lowercase();
                !kw_clean.is_empty() && lower_body.contains(&kw_clean)
            }).cloned()
        };

        matched_keyword.map(|kw| {
            format!("Die Nachricht enthält das Wort „{}“, aber es wurde kein Anhang hinzugefügt. Trotzdem senden?", kw)
        })
    }

    /// Runtime hook: parse spam headers from incoming mail based on active junk plugin.
    pub fn evaluate_spam_headers(
        headers: &[(String, String)],
        active_plugin: &str,
    ) -> Option<bool> {
        let plugin_lower = active_plugin.to_lowercase();
        for (name, val) in headers {
            let name_lower = name.to_lowercase();
            let val_lower = val.to_lowercase();

            if plugin_lower.contains("spamassassin") || plugin_lower.contains("sa") {
                if name_lower == "x-spam-status" || name_lower == "x-spam-flag" {
                    return Some(val_lower.starts_with("yes") || val_lower.contains("is spam"));
                }
            } else if plugin_lower.contains("bogofilter") || plugin_lower.contains("bogo") {
                if name_lower == "x-bogosity" {
                    return Some(val_lower.starts_with("yes") || val_lower.starts_with("spam"));
                }
            } else if name_lower == "x-spam-flag" || name_lower == "x-spam-status" || name_lower == "x-bogosity" {
                return Some(val_lower.starts_with("yes") || val_lower.starts_with("spam"));
            }
        }
        None
    }

    /// Runtime hook: format external editor command string safely.
    pub fn format_external_editor_command(
        command_template: &str,
        file_path: &Path,
    ) -> Vec<String> {
        let path_str = file_path.to_string_lossy().to_string();
        let template = command_template.trim();
        if template.is_empty() {
            return vec!["gnome-text-editor".to_string(), path_str];
        }

        if template.contains("%s") {
            let formatted = template.replace("%s", &path_str);
            formatted.split_whitespace().map(String::from).collect()
        } else {
            let mut parts: Vec<String> = template.split_whitespace().map(String::from).collect();
            parts.push(path_str);
            parts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_registry_and_categories() {
        let plugins = PluginManager::all_plugins();
        assert_eq!(plugins.len(), 8);

        let reminder = PluginManager::find_plugin("org.gnome.evolution.plugin.attachment-reminder").unwrap();
        assert_eq!(reminder.name, "Anhang-Erinnerung");
        assert_eq!(reminder.category, PluginCategory::Mail);
        assert!(reminder.is_configurable);

        let itip = PluginManager::find_plugin("org.gnome.evolution.plugin.itip-formatter").unwrap();
        assert_eq!(itip.category, PluginCategory::Calendar);
    }

    #[test]
    fn plugin_enable_disable_toggle() {
        let mut disabled = Vec::new();
        let plugin_id = "org.gnome.evolution.plugin.bogofilter";

        assert!(PluginManager::is_plugin_enabled(plugin_id, &disabled));

        // Disable
        assert!(PluginManager::disable_plugin(plugin_id, &mut disabled));
        assert!(!PluginManager::is_plugin_enabled(plugin_id, &disabled));
        assert_eq!(disabled.len(), 1);

        // Toggle back to enabled
        let state = PluginManager::toggle_plugin(plugin_id, &mut disabled);
        assert!(state);
        assert!(PluginManager::is_plugin_enabled(plugin_id, &disabled));
        assert_eq!(disabled.len(), 0);
    }

    #[test]
    fn attachment_reminder_detection() {
        let body_with_keyword = "Hallo,\n\nanbei sende ich Ihnen die Rechnung als PDF.\n\nViele Grüße";
        let warning = PluginManager::check_missing_attachment(body_with_keyword, false, true, &[]);
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("anbei"));

        // When attachment is present, no warning
        let no_warn = PluginManager::check_missing_attachment(body_with_keyword, true, true, &[]);
        assert!(no_warn.is_none());

        // When reminder is disabled, no warning
        let disabled_warn = PluginManager::check_missing_attachment(body_with_keyword, false, false, &[]);
        assert!(disabled_warn.is_none());

        // Custom keyword
        let body_cv = "Please find my CV in this email.";
        let cv_warn = PluginManager::check_missing_attachment(body_cv, false, true, &["cv".to_string()]);
        assert!(cv_warn.is_some());
        assert!(cv_warn.unwrap().contains("cv"));
    }

    #[test]
    fn spam_header_evaluation() {
        let sa_headers = vec![
            ("Subject".to_string(), "Free Offer".to_string()),
            ("X-Spam-Status".to_string(), "Yes, score=12.5 required=5.0".to_string()),
        ];
        assert_eq!(
            PluginManager::evaluate_spam_headers(&sa_headers, "SpamAssassin"),
            Some(true)
        );

        let bogo_headers = vec![
            ("Subject".to_string(), "Hello friend".to_string()),
            ("X-Bogosity".to_string(), "Unsure, tests=bogofilter".to_string()),
        ];
        assert_eq!(
            PluginManager::evaluate_spam_headers(&bogo_headers, "Bogofilter"),
            Some(false)
        );
    }

    #[test]
    fn external_editor_command_formatting() {
        let path = Path::new("/tmp/evolution-draft-123.eml");
        let cmd = PluginManager::format_external_editor_command("gedit --new-window %s", path);
        assert_eq!(cmd, vec!["gedit", "--new-window", "/tmp/evolution-draft-123.eml"]);

        let default_cmd = PluginManager::format_external_editor_command("", path);
        assert_eq!(default_cmd, vec!["gnome-text-editor", "/tmp/evolution-draft-123.eml"]);
    }
}
