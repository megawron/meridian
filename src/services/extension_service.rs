// SPDX-License-Identifier: GPL-3.0-or-later
//
// ExtensionService — Declarative contribution points and sandboxed command runner
// for Meridian add-ons ("simple core, flexible via extensions").
//
// Extensions declare contribution points in `extension.json` (modeled after
// VS Code's `contributes` manifest and Obsidian's CSS/plugin model):
// - `commands`: executable actions invoked from the Command Palette, buttons, or menus
// - `header_actions`: symbolic icon buttons mounted in Mail, Composer, Calendar, Tasks, or Contacts
// - `sidebar_items`: custom navigation items / panels in the Mail & Shell sidebar
// - `message_actions`: contextual actions on selected messages
// - `composer_hooks`: draft transformation or pre-send hooks
// - `views`: custom top-level shell tabs (WebView URL, local HTML app, or full mini-browser)
// - `stylesheets`: GTK4 shell CSS and MailViewer CSS customizations
// - `message_transformers`: declarative regex rules for MailViewer HTML, subjects, and list badges
// - `attachment_renderers`: inline document renderers (Markdown, CSV, JSON, E-Invoice XML, or scripts)
// - `locales`: community JSON translation packs layered onto Meridian's i18n lookup
//
// Both bundled optional add-ons (disabled by default to keep the core UI minimal)
// and user-installed folder extensions under `<app_data_dir>/extensions/<id>/`
// share this exact contract.

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const STATE_VERSION: u32 = 1;
const RSS_STORE_VERSION: u32 = 1;
const MAX_SCRIPT_OUTPUT_BYTES: usize = 512 * 1024;

static EXTENSION_REVISION: AtomicU64 = AtomicU64::new(1);
static EXTENSION_LISTENERS: Lazy<Mutex<Vec<async_channel::Sender<u64>>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

struct CompiledListTransformers {
    revision: u64,
    subject_rules: Vec<(Regex, String)>,
    badge_rules: Vec<(Regex, String)>,
}

static LIST_TRANSFORMER_CACHE: Lazy<Mutex<Option<CompiledListTransformers>>> =
    Lazy::new(|| Mutex::new(None));

fn with_cached_list_transformers<R>(f: impl FnOnce(&CompiledListTransformers) -> R) -> R {
    let rev = current_extension_revision();
    let mut guard = LIST_TRANSFORMER_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let needs_rebuild = guard.as_ref().is_none_or(|c| c.revision != rev);
    if needs_rebuild {
        let reg = ExtensionRegistry::default();
        let subject_rules = reg
            .enabled_message_transformers(TransformerTarget::Subject)
            .into_iter()
            .filter_map(|(_, r)| Regex::new(&r.pattern).ok().map(|re| (re, r.replacement)))
            .collect();
        let badge_rules = reg
            .enabled_message_transformers(TransformerTarget::ListBadge)
            .into_iter()
            .filter_map(|(_, r)| Regex::new(&r.pattern).ok().map(|re| (re, r.replacement)))
            .collect();
        *guard = Some(CompiledListTransformers {
            revision: rev,
            subject_rules,
            badge_rules,
        });
    }
    f(guard.as_ref().expect("cache populated"))
}

/// Fast O(1)-cached subject transformation for Mail list rendering.
pub fn apply_cached_subject_transformers(subject: &str) -> String {
    with_cached_list_transformers(|cache| {
        if cache.subject_rules.is_empty() {
            return subject.to_string();
        }
        let mut current = subject.to_string();
        for (re, repl) in &cache.subject_rules {
            current = re.replace_all(&current, repl.as_str()).into_owned();
        }
        current
    })
}

/// Fast O(1)-cached badge label extraction for Mail list rendering.
pub fn cached_message_list_badges(subject: &str, sender: &str) -> Vec<String> {
    with_cached_list_transformers(|cache| {
        if cache.badge_rules.is_empty() {
            return Vec::new();
        }
        let haystack = format!("{subject}\n{sender}");
        let mut badges = Vec::new();
        for (re, repl) in &cache.badge_rules {
            if let Some(caps) = re.captures(&haystack) {
                let mut label = String::new();
                caps.expand(repl, &mut label);
                let label = label.trim().to_string();
                if !label.is_empty() {
                    badges.push(label);
                }
            }
        }
        badges
    })
}

/// Target UI surface where an extension button can appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionSurface {
    Mail,
    Composer,
    Calendar,
    Contacts,
    Tasks,
}

impl ExtensionSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Composer => "composer",
            Self::Calendar => "calendar",
            Self::Contacts => "contacts",
            Self::Tasks => "tasks",
        }
    }
}

/// Kind of panel rendered when a contributed sidebar item is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPanelKind {
    #[default]
    CommandOutput,
    RssFeeds,
    ActivityMonitor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCommandContribution {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub shortcut: String,
    /// Relative executable/script inside the extension folder (or `builtin:<id>`).
    #[serde(default)]
    pub exec: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionHeaderActionContribution {
    pub id: String,
    pub command: String,
    pub icon_name: String,
    pub tooltip: String,
    pub surface: ExtensionSurface,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSidebarContribution {
    pub id: String,
    pub title: String,
    pub icon_name: String,
    #[serde(default)]
    pub panel_kind: SidebarPanelKind,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMessageActionContribution {
    pub id: String,
    pub command: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionComposerHook {
    /// `"transform"` or `"pre_send"`.
    pub event: String,
    pub command: String,
}

fn default_true() -> bool {
    true
}

/// Where an extension's WebView is mounted in Meridian's shell.
/// Extension authors pick the sensible default; users can also override it in Extensions UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionViewPlacement {
    /// Top-level workspace tab in the main ViewSwitcher.
    #[default]
    Tab,
    /// Global right-hand collapsible sidebar shared across Mail, Calendar, Contacts, and Tasks.
    GlobalSidebar,
    /// Page-specific right-hand collapsible sidebar shown only on `surfaces` (defaults to Mail).
    PageSidebar,
}

/// Custom workspace view contributed by an extension (top-level tab, global right sidebar,
/// or page-specific right sidebar — loading an external URL or a local HTML/JS app).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionViewContribution {
    pub id: String,
    pub title: String,
    #[serde(default = "default_extension_icon")]
    pub icon_name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub html_file: String,
    #[serde(default)]
    pub show_navigation_bar: bool,
    #[serde(default = "default_true")]
    pub allow_javascript: bool,
    #[serde(default)]
    pub placement: ExtensionViewPlacement,
    /// Surfaces where `PageSidebar` appears (e.g. `["mail", "calendar"]`). Defaults to `["mail"]`.
    #[serde(default)]
    pub surfaces: Vec<ExtensionSurface>,
    /// Optional preferred sidebar width in pixels (`260..=640`, default `360`).
    #[serde(default)]
    pub default_width: Option<i32>,
}

/// Target surface for a custom CSS stylesheet contributed by an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StylesheetTarget {
    #[default]
    App,
    MailViewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStylesheetContribution {
    #[serde(default)]
    pub target: StylesheetTarget,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub css: String,
}

/// Target surface for a declarative regex transformer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransformerTarget {
    #[default]
    BodyHtml,
    Subject,
    ListBadge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMessageTransformer {
    pub id: String,
    #[serde(default)]
    pub target: TransformerTarget,
    pub pattern: String,
    pub replacement: String,
    #[serde(default)]
    pub badge_color: String,
}

/// Inline attachment document renderer contributed by an extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionAttachmentRenderer {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub exec: String,
}

/// Community language pack / UI translation override contributed by an extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLocaleContribution {
    /// Locale code such as `"ja"`, `"ko"`, `"zh_CN"`, `"de"`, or `"*"` for always-active overrides.
    pub code: String,
    pub name: String,
    pub json_file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionSettingKind {
    #[default]
    String,
    Bool,
    /// Stored encrypted in the OS keyring (`libsecret` / GNOME Keyring), never plaintext JSON.
    Secret,
}

/// Declarative setting rendered automatically in the Extensions UI (`contributes.settings`).
/// Referenced in URLs/patterns via `{{setting:<key>}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSettingSchema {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: ExtensionSettingKind,
    #[serde(default)]
    pub default_value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionContributions {
    #[serde(default)]
    pub commands: Vec<ExtensionCommandContribution>,
    #[serde(default)]
    pub header_actions: Vec<ExtensionHeaderActionContribution>,
    #[serde(default)]
    pub sidebar_items: Vec<ExtensionSidebarContribution>,
    #[serde(default)]
    pub message_actions: Vec<ExtensionMessageActionContribution>,
    #[serde(default)]
    pub composer_hooks: Vec<ExtensionComposerHook>,
    #[serde(default)]
    pub views: Vec<ExtensionViewContribution>,
    #[serde(default)]
    pub stylesheets: Vec<ExtensionStylesheetContribution>,
    #[serde(default)]
    pub message_transformers: Vec<ExtensionMessageTransformer>,
    #[serde(default)]
    pub attachment_renderers: Vec<ExtensionAttachmentRenderer>,
    #[serde(default)]
    pub locales: Vec<ExtensionLocaleContribution>,
    #[serde(default)]
    pub settings: Vec<ExtensionSettingSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_extension_icon")]
    pub icon_name: String,
    #[serde(default)]
    pub enabled_by_default: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub contributes: ExtensionContributions,
}

fn default_extension_icon() -> String {
    "application-x-addon-symbolic".to_string()
}

/// Risk tier of an extension permission, used to present clear Chrome / GNOME-Software-style
/// trust indicators before installing or enabling an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionRiskLevel {
    Standard,
    Sensitive,
    HighRisk,
}

/// Canonical, runtime-enforced capability scopes for Meridian extensions.
/// Ordered from highest risk to lowest risk for clear user review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtensionPermission {
    /// Execute local scripts or external programs on the host (`shell`).
    Shell,
    /// Read the body text / HTML of opened emails (`mail.body`).
    ReadMailBody,
    /// Read and inspect email attachments (`mail.attachments`).
    ReadMailAttachments,
    /// Read email headers: sender, recipients, subject, date (`mail.headers`).
    ReadMailHeaders,
    /// Modify displayed email HTML, subjects, or list badges (`mail.modify`).
    ModifyMailDisplay,
    /// Open/prefill reply drafts or run composer hooks (`mail.compose`).
    ComposeMail,
    /// Create or modify tasks in Meridian (`tasks.write`).
    WriteTasks,
    /// Connect to external servers, load remote web pages, or open links (`network`).
    Network,
    /// Customize application/email CSS stylesheets or UI translations (`ui.style`).
    UiCustomize,
}

impl ExtensionPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::ReadMailBody => "mail.body",
            Self::ReadMailAttachments => "mail.attachments",
            Self::ReadMailHeaders => "mail.headers",
            Self::ModifyMailDisplay => "mail.modify",
            Self::ComposeMail => "mail.compose",
            Self::WriteTasks => "tasks.write",
            Self::Network => "network",
            Self::UiCustomize => "ui.style",
        }
    }

    pub fn risk_level(self) -> PermissionRiskLevel {
        match self {
            Self::Shell => PermissionRiskLevel::HighRisk,
            Self::ReadMailBody
            | Self::ReadMailAttachments
            | Self::ReadMailHeaders
            | Self::ModifyMailDisplay
            | Self::ComposeMail
            | Self::WriteTasks
            | Self::Network => PermissionRiskLevel::Sensitive,
            Self::UiCustomize => PermissionRiskLevel::Standard,
        }
    }

    /// Map a manifest permission string (granular v2 or legacy v1 alias) to canonical scopes.
    pub fn from_manifest_token(raw: &str) -> Vec<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shell" => vec![Self::Shell],
            "mail.headers" => vec![Self::ReadMailHeaders],
            "mail.body" => vec![Self::ReadMailBody],
            "mail.attachments" => vec![Self::ReadMailAttachments],
            "mail.modify" => vec![Self::ModifyMailDisplay],
            "mail.compose" => vec![Self::ComposeMail],
            "tasks.write" => vec![Self::WriteTasks],
            "network" => vec![Self::Network],
            "ui.style" | "ui_css" => vec![Self::UiCustomize],
            // Legacy v1 coarse aliases expanded into their exact underlying capabilities:
            "read_mail" => vec![
                Self::ReadMailHeaders,
                Self::ReadMailBody,
                Self::ReadMailAttachments,
            ],
            "modify_mail" => vec![
                Self::ModifyMailDisplay,
                Self::ComposeMail,
                Self::WriteTasks,
            ],
            _ => Vec::new(),
        }
    }

    /// Normalize, deduplicate, and sort raw manifest permission tokens by risk severity.
    pub fn normalize_list(raw_tokens: &[String]) -> Vec<Self> {
        let mut set = std::collections::BTreeSet::new();
        for token in raw_tokens {
            for perm in Self::from_manifest_token(token) {
                set.insert(perm);
            }
        }
        set.into_iter().collect()
    }

    pub fn overall_risk(perms: &[Self]) -> PermissionRiskLevel {
        perms
            .iter()
            .map(|p| p.risk_level())
            .max()
            .unwrap_or(PermissionRiskLevel::Standard)
    }
}

impl ExtensionManifest {
    /// Canonical permissions explicitly declared in `extension.json`.
    pub fn declared_permissions(&self) -> Vec<ExtensionPermission> {
        ExtensionPermission::normalize_list(&self.permissions)
    }

    /// Capabilities implied by the extension's actual `contributes` entries.
    pub fn contributed_capabilities(&self) -> Vec<ExtensionPermission> {
        let mut set = std::collections::BTreeSet::new();
        let c = &self.contributes;

        for cmd in &c.commands {
            let exec = cmd.exec.trim();
            if !exec.is_empty() && !exec.starts_with("builtin:") {
                set.insert(ExtensionPermission::Shell);
            }
        }
        if !c.attachment_renderers.is_empty() {
            set.insert(ExtensionPermission::ReadMailAttachments);
            for renderer in &c.attachment_renderers {
                let exec = renderer.exec.trim();
                if !exec.is_empty() && !exec.starts_with("builtin:") {
                    set.insert(ExtensionPermission::Shell);
                }
            }
        }
        for rule in &c.message_transformers {
            set.insert(ExtensionPermission::ModifyMailDisplay);
            match rule.target {
                TransformerTarget::BodyHtml => {
                    set.insert(ExtensionPermission::ReadMailBody);
                }
                TransformerTarget::Subject | TransformerTarget::ListBadge => {
                    set.insert(ExtensionPermission::ReadMailHeaders);
                }
            }
        }
        if !c.composer_hooks.is_empty() {
            set.insert(ExtensionPermission::ComposeMail);
        }
        if !c.stylesheets.is_empty() || !c.locales.is_empty() {
            set.insert(ExtensionPermission::UiCustomize);
        }
        for view in &c.views {
            if !view.url.trim().is_empty() {
                set.insert(ExtensionPermission::Network);
            }
        }
        set.into_iter().collect()
    }

    /// Complete audited permission set shown to users before installing or enabling:
    /// union of explicitly declared permissions and capabilities actually required by `contributes`.
    pub fn audited_permissions(&self) -> Vec<ExtensionPermission> {
        let mut set = std::collections::BTreeSet::new();
        for p in self.declared_permissions() {
            set.insert(p);
        }
        for p in self.contributed_capabilities() {
            set.insert(p);
        }
        set.into_iter().collect()
    }

    /// Capabilities used in `contributes` that were omitted from `permissions` (blocked at runtime).
    pub fn undeclared_capabilities(&self) -> Vec<ExtensionPermission> {
        let declared: HashSet<ExtensionPermission> =
            self.declared_permissions().into_iter().collect();
        self.contributed_capabilities()
            .into_iter()
            .filter(|p| !declared.contains(p))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledExtension {
    pub manifest: ExtensionManifest,
    pub root_dir: Option<PathBuf>,
    pub is_bundled: bool,
    pub enabled: bool,
}

impl InstalledExtension {
    /// Returns true if this extension explicitly declared `permission` (supporting both
    /// canonical granular scopes like `"mail.attachments"` and legacy aliases like `"read_mail"`).
    pub fn has_permission(&self, permission: &str) -> bool {
        let required = ExtensionPermission::from_manifest_token(permission);
        if required.is_empty() {
            return self
                .manifest
                .permissions
                .iter()
                .any(|p| p.trim().eq_ignore_ascii_case(permission));
        }
        let declared = self.manifest.declared_permissions();
        required.iter().all(|req| declared.contains(req))
    }

    /// Returns true if this extension explicitly declared the typed capability `permission`.
    pub fn has_permission_kind(&self, permission: ExtensionPermission) -> bool {
        self.manifest.declared_permissions().contains(&permission)
    }

    /// Full audited capability list (declared + implied by `contributes`) for user trust review.
    pub fn audited_permissions(&self) -> Vec<ExtensionPermission> {
        self.manifest.audited_permissions()
    }
}

/// JSON payload passed on stdin to external extension commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionExecutionContext {
    pub command_id: String,
    pub active_surface: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub message_subject: Option<String>,
    #[serde(default)]
    pub message_sender: Option<String>,
    #[serde(default)]
    pub message_recipients: Option<String>,
    #[serde(default)]
    pub message_date: Option<String>,
    #[serde(default)]
    pub message_body_text: Option<String>,
    #[serde(default)]
    pub composer_to: Option<String>,
    #[serde(default)]
    pub composer_subject: Option<String>,
    #[serde(default)]
    pub composer_body_html: Option<String>,
}

/// Structured host mutations returned on stdout by an extension command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionHostResponse {
    #[serde(default)]
    pub toast: Option<String>,
    #[serde(default)]
    pub open_url: Option<String>,
    #[serde(default)]
    pub insert_composer_html: Option<String>,
    #[serde(default)]
    pub replace_composer_html: Option<String>,
    #[serde(default)]
    pub replace_composer_subject: Option<String>,
    #[serde(default)]
    pub add_label: Option<String>,
    #[serde(default)]
    pub create_task_title: Option<String>,
    #[serde(default)]
    pub output_title: Option<String>,
    #[serde(default)]
    pub output_body: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ExtensionStateFile {
    version: u32,
    #[serde(default)]
    enabled_overrides: BTreeMap<String, bool>,
    #[serde(default)]
    settings_values: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    view_placement_overrides: BTreeMap<String, ExtensionViewPlacement>,
}

pub fn extensions_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("MERIDIAN_EXTENSIONS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return custom;
    }
    super::app_data_dir().join("extensions")
}

/// Concise, self-contained extension authoring guide optimized for both humans and LLMs.
/// Automatically written to `<extensions_dir>/EXTENSION_GUIDE.md` so users can point any
/// AI coding assistant at the folder and generate working Meridian extensions immediately.
pub const EXTENSION_DEV_GUIDE_MD: &str = r##"# Meridian Extension Development Guide (LLM & Developer Reference)

Create a folder inside Meridian's extensions directory containing an `extension.json` file:
- Native Linux: `~/.local/share/meridian/extensions/<your-extension-id>/extension.json`
- Flatpak: `~/.var/app/com.martingawron.Meridian/data/meridian/extensions/<your-extension-id>/extension.json`

Meridian hot-reloads extensions when toggled, installed, or configured in **Menu → Extensions**.

## 1. Minimal `extension.json` Schema

```json
{
  "id": "author.my-extension",
  "name": "My Extension",
  "version": "1.0.0",
  "author": "Your Name",
  "description": "Short explanation of what this extension does.",
  "icon_name": "application-x-addon-symbolic",
  "enabled_by_default": true,
  "permissions": ["mail.headers", "mail.body", "mail.modify"],
  "contributes": {
    "views": [],
    "stylesheets": [],
    "message_transformers": [],
    "attachment_renderers": [],
    "locales": [],
    "settings": [],
    "commands": [],
    "header_actions": [],
    "sidebar_items": [],
    "message_actions": [],
    "composer_hooks": []
  }
}
```

- `id`: ASCII letters, digits, `.`, `-`, `_` only (1–128 chars).
- `permissions` *(strictly enforced at runtime and reviewed by the user before install/enable)*:
  - `"mail.headers"`: read sender, recipients, subject, and headers (required for `subject`/`list_badge` rules and header fields in `getSelectedMail` / command stdin)
  - `"mail.body"`: read opened email body text/HTML (required for `body_html` rules and `body_text` in `getSelectedMail` / command stdin)
  - `"mail.attachments"`: read & render email attachments (required for `attachment_renderers`)
  - `"mail.modify"`: modify email display HTML, subjects, or list badges (required for `message_transformers`)
  - `"mail.compose"`: open reply drafts & access composer fields (required for `openReplyDraft` and `composer_hooks`)
  - `"tasks.write"`: create tasks via `createTask`
  - `"network"`: connect to external servers, load remote WebView URLs, or call `openUrl`
  - `"ui.style"`: load CSS stylesheets or language overrides
  - `"shell"`: *(High Risk)* execute local scripts in `commands` or external `attachment_renderers`
- Verified Adwaita symbolic icons: `application-x-addon-symbolic`, `mail-message-new-symbolic`, `web-browser-symbolic`, `x-office-document-symbolic`, `edit-find-replace-symbolic`, `applications-graphics-symbolic`, `preferences-desktop-locale-symbolic`, `utilities-terminal-symbolic`, `application-rss+xml-symbolic`, `system-users-symbolic`, `starred-symbolic`, `folder-symbolic`.

## 2. Contribution Points

### A. Custom Workspace Views (`views` — Tabs, Global Sidebar, or Page-Specific Sidebar)
Developers choose where the view mounts by default (`placement`), and users can override it on the extension's detail page:
- `"tab"`: top-level tab alongside Mail, Calendar, Contacts, and Tasks (default).
- `"global_sidebar"`: collapsible right-hand sidebar shared across all Meridian views (great for AI chat, notes, or timers).
- `"page_sidebar"`: collapsible right-hand sidebar shown only on `surfaces` (e.g. `["mail"]` or `["mail", "calendar"]`).
```json
"views": [
  {
    "id": "ai-assistant",
    "title": "AI Assistant",
    "icon_name": "mail-message-new-symbolic",
    "url": "",
    "html_file": "index.html",
    "placement": "global_sidebar",
    "surfaces": ["mail"],
    "default_width": 380,
    "show_navigation_bar": false,
    "allow_javascript": true
  }
]
```
- Use `url` (`https://...`) for web apps (Element/Matrix, Nextcloud, Zulip, etc.) or set `"show_navigation_bar": true` for a Back/Forward/Reload/URL bar.
- Or leave `url` empty and set `html_file` to a relative file inside the extension folder (e.g. `"index.html"`).

#### JavaScript Host Bridge (`window.meridian.*`) for Local `html_file` Views
Local HTML/JS extension views automatically receive an async `window.meridian` API with **enforced permissions** (and strict `connect-src 'none'` CSP unless `"network"` is declared):
- `await window.meridian.getContext()` → `{ active_view, extension_id, permissions }`
- `await window.meridian.getSelectedMail()` *(requires `"mail.headers"` or `"mail.body"`; `body_text` is only included with `"mail.body"`)* → `{ id, subject, sender, recipients, body_text }` or `null`
- `await window.meridian.getSetting(key)` → reads any declared `string`, `bool`, or keyring-backed `secret` setting (e.g. an LLM API key)
- `await window.meridian.openReplyDraft(html)` *(requires `"mail.compose"`)* → opens Meridian's Composer prefilled as a reply
- `await window.meridian.createTask(title)` *(requires `"tasks.write"`)* → creates a task in the default task list
- `await window.meridian.openUrl(url)` *(requires `"network"`)* → opens an `https://` link in the system browser
- `await window.meridian.showToast(message)` → displays a native toast notification

### B. User-Editable Settings (`settings`, including Keyring Secrets)
Meridian automatically renders a native libadwaita settings UI on the extension's detail page.
Reference string values in `views[].url`, `views[].title`, `message_transformers[].pattern`, or `replacement` using `{{setting:<key>}}`.
```json
"settings": [
  {
    "key": "api_key",
    "title": "API Key",
    "description": "Stored encrypted in the OS keyring, never in plaintext JSON.",
    "kind": "secret",
    "default_value": ""
  },
  {
    "key": "server_url",
    "title": "Server URL",
    "description": "Web app or API endpoint URL.",
    "kind": "string",
    "default_value": "https://app.element.io"
  }
]
```
`kind`: `"string"`, `"bool"` (`"true"` / `"false"`), or `"secret"` (masked `PasswordEntryRow` saved in GNOME Keyring / `libsecret`).

### C. Custom CSS (`stylesheets`)
Target either `"app"` (GTK4 shell CSS) or `"mail_viewer"` (injected into HTML email previews).
```json
"stylesheets": [
  { "target": "app", "path": "app.css", "css": "" },
  { "target": "mail_viewer", "path": "viewer.css", "css": "" }
]
```

### D. Declarative Regex Rules (`message_transformers`)
Apply Rust regex rules without writing any scripts!
- `target`: `"body_html"` (safely transforms text nodes outside HTML tags), `"subject"` (rewrites displayed subject in Mail list), or `"list_badge"` (adds a badge chip in the Mail list from capture group `$1`).
```json
"message_transformers": [
  {
    "id": "ticket-highlight",
    "target": "body_html",
    "pattern": "\\b([A-Z]{2,8}-\\d{1,6})\\b",
    "replacement": "<span style=\"padding:1px 6px;border-radius:6px;background:rgba(53,132,228,0.16);color:#3584e4;font-weight:600;\">$1</span>"
  },
  {
    "id": "invoice-badge",
    "target": "list_badge",
    "pattern": "(?i)\\b(INVOICE|RECHNUNG)\\b",
    "replacement": "$1",
    "badge_color": "#e01b24"
  }
]
```

### E. Inline Attachment Document Renderers (`attachment_renderers`)
Render attachments directly inside the MailViewer below the email body.
```json
"attachment_renderers": [
  {
    "id": "custom-yaml",
    "title": "YAML Preview",
    "extensions": ["yml", "yaml"],
    "mime_types": ["application/yaml", "text/yaml"],
    "exec": "render_yaml.py"
  }
]
```
- Built-in renderers: `"builtin:markdown"`, `"builtin:csv"`, `"builtin:json"`, `"builtin:e-invoice"`.
- Custom script (`exec`): receives JSON `{"filename","mime_type","content_utf8"}` on `stdin` and prints sanitized HTML fragment on `stdout` (5s timeout).

### F. Community Language Packs (`locales`)
Override or add any UI language via a simple English-to-target JSON dictionary:
```json
"locales": [
  { "code": "ja", "name": "Japanese", "json_file": "ja.json" }
]
```
Use `"code": "*"` to apply overrides regardless of system locale.

### G. Executable Commands, Header Buttons & Message Actions (`commands`, `header_actions`, `message_actions`, `sidebar_items`)
- Scripts receive JSON `ExtensionExecutionContext` on `stdin`:
  `{ "command_id", "active_surface", "message_id", "message_subject", "message_sender", "message_recipients", "message_date", "message_body_text", "composer_to", "composer_subject", "composer_body_html" }`
- Scripts print JSON `ExtensionHostResponse` on `stdout`:
  `{ "toast", "open_url", "insert_composer_html", "replace_composer_html", "replace_composer_subject", "add_label", "create_task_title", "output_title", "output_body" }`
- `header_actions[].surface`: `"mail"`, `"composer"`, `"calendar"`, `"contacts"`, or `"tasks"`.
- Remember to make scripts executable (`chmod +x script.py`) with a valid shebang (`#!/usr/bin/env python3`).
"##;

/// Ensure `<ext_dir>/EXTENSION_GUIDE.md` exists and is up to date so developers and LLMs
/// always have complete, accurate documentation right inside the extensions directory.
pub fn ensure_extension_guide_written(ext_dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(ext_dir)?;
    let guide_path = ext_dir.join("EXTENSION_GUIDE.md");
    let needs_write = fs::read_to_string(&guide_path)
        .map(|existing| existing != EXTENSION_DEV_GUIDE_MD)
        .unwrap_or(true);
    if needs_write {
        fs::write(&guide_path, EXTENSION_DEV_GUIDE_MD)?;
    }
    Ok(guide_path)
}

fn state_file_path() -> PathBuf {
    if let Some(custom) = std::env::var_os("MERIDIAN_EXTENSIONS_STATE_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return custom;
    }
    super::app_data_dir().join("extensions-state.json")
}

pub fn current_extension_revision() -> u64 {
    EXTENSION_REVISION.load(Ordering::SeqCst)
}

pub fn notify_extensions_changed() {
    let rev = EXTENSION_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut listeners) = EXTENSION_LISTENERS.lock() {
        listeners.retain(|tx| tx.send_blocking(rev).is_ok());
    }
}

pub fn subscribe_extension_updates() -> async_channel::Receiver<u64> {
    let (tx, rx) = async_channel::unbounded();
    if let Ok(mut listeners) = EXTENSION_LISTENERS.lock() {
        listeners.push(tx);
    }
    rx
}

/// Optional built-in add-ons shipped with Meridian.
/// Disabled by default so the core UI remains clean and un-overwhelming.
pub fn bundled_extensions() -> Vec<ExtensionManifest> {
    vec![
        ExtensionManifest {
            id: "meridian.rss-feeds".to_string(),
            name: "RSS & Atom Feeds".to_string(),
            version: "1.1.0".to_string(),
            author: "Martin Gawron".to_string(),
            description: "Adds a Feeds section to the mail sidebar and renders RSS and Atom articles directly in the mail list and message preview.".to_string(),
            icon_name: "application-rss+xml-symbolic".to_string(),
            enabled_by_default: false,
            permissions: vec!["network".to_string()],
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "rss.manage-feeds".to_string(),
                    title: "Feeds: Add or Manage Subscriptions…".to_string(),
                    category: "Feeds".to_string(),
                    shortcut: String::new(),
                    exec: "builtin:rss.manage".to_string(),
                    args: Vec::new(),
                }],
                sidebar_items: vec![ExtensionSidebarContribution {
                    id: "meridian.sidebar.rss".to_string(),
                    title: "Feeds".to_string(),
                    icon_name: "application-rss+xml-symbolic".to_string(),
                    panel_kind: SidebarPanelKind::RssFeeds,
                    command: "rss.manage-feeds".to_string(),
                }],
                ..ExtensionContributions::default()
            },
        },
        ExtensionManifest {
            id: "meridian.mail-merge".to_string(),
            name: "Mail Merge".to_string(),
            version: "1.0.0".to_string(),
            author: "Martin Gawron".to_string(),
            description: "Personalizes outgoing messages per recipient using {{name}}, {{first_name}}, and {{email}} placeholders.".to_string(),
            icon_name: "system-users-symbolic".to_string(),
            enabled_by_default: false,
            permissions: vec!["mail.compose".to_string()],
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "mail-merge.insert-placeholder".to_string(),
                    title: "Mail Merge: Insert Recipient Variable".to_string(),
                    category: "Composer".to_string(),
                    shortcut: String::new(),
                    exec: "builtin:mail-merge.insert".to_string(),
                    args: Vec::new(),
                }],
                header_actions: vec![ExtensionHeaderActionContribution {
                    id: "mail-merge.composer-btn".to_string(),
                    command: "mail-merge.insert-placeholder".to_string(),
                    icon_name: "system-users-symbolic".to_string(),
                    tooltip: "Insert Mail Merge placeholder ({{first_name}}, {{email}})".to_string(),
                    surface: ExtensionSurface::Composer,
                }],
                ..ExtensionContributions::default()
            },
        },
        ExtensionManifest {
            id: "meridian.activity-monitor".to_string(),
            name: "Activity Monitor".to_string(),
            version: "1.0.0".to_string(),
            author: "Martin Gawron".to_string(),
            description: "Adds a quick header button and activity log view to inspect background sync, filter, and transport operations.".to_string(),
            icon_name: "utilities-terminal-symbolic".to_string(),
            enabled_by_default: false,
            permissions: Vec::new(),
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "activity.open-monitor".to_string(),
                    title: "Open Activity Monitor".to_string(),
                    category: "Diagnostics".to_string(),
                    shortcut: String::new(),
                    exec: "builtin:activity.open".to_string(),
                    args: Vec::new(),
                }],
                header_actions: vec![ExtensionHeaderActionContribution {
                    id: "activity.mail-header-btn".to_string(),
                    command: "activity.open-monitor".to_string(),
                    icon_name: "utilities-terminal-symbolic".to_string(),
                    tooltip: "Activity Monitor".to_string(),
                    surface: ExtensionSurface::Mail,
                }],
                ..ExtensionContributions::default()
            },
        },
    ]
}

#[derive(Debug, Clone)]
pub struct ExtensionRegistry {
    ext_dir: PathBuf,
    state_path: PathBuf,
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self {
            ext_dir: extensions_dir(),
            state_path: state_file_path(),
        }
    }
}

impl ExtensionRegistry {
    pub fn new(ext_dir: impl Into<PathBuf>, state_path: impl Into<PathBuf>) -> Self {
        Self {
            ext_dir: ext_dir.into(),
            state_path: state_path.into(),
        }
    }

    pub fn extensions_directory(&self) -> &Path {
        &self.ext_dir
    }

    fn load_state(&self) -> ExtensionStateFile {
        let Ok(bytes) = fs::read(&self.state_path) else {
            return ExtensionStateFile {
                version: STATE_VERSION,
                enabled_overrides: BTreeMap::new(),
                settings_values: BTreeMap::new(),
                view_placement_overrides: BTreeMap::new(),
            };
        };
        serde_json::from_slice::<ExtensionStateFile>(&bytes).unwrap_or(ExtensionStateFile {
            version: STATE_VERSION,
            enabled_overrides: BTreeMap::new(),
            settings_values: BTreeMap::new(),
            view_placement_overrides: BTreeMap::new(),
        })
    }

    fn save_state(&self, state: &ExtensionStateFile) -> io::Result<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(&ExtensionStateFile {
            version: STATE_VERSION,
            enabled_overrides: state.enabled_overrides.clone(),
            settings_values: state.settings_values.clone(),
            view_placement_overrides: state.view_placement_overrides.clone(),
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.state_path.with_extension("json.tmp");
        fs::write(&tmp, payload)?;
        fs::rename(tmp, &self.state_path)
    }

    pub fn extension_settings_values(&self, extension_id: &str) -> BTreeMap<String, String> {
        self.load_state()
            .settings_values
            .remove(extension_id)
            .unwrap_or_default()
    }

    pub fn set_extension_setting(
        &self,
        extension_id: &str,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let mut state = self.load_state();
        state
            .settings_values
            .entry(extension_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save_state(&state)?;
        notify_extensions_changed();
        Ok(())
    }

    fn secret_keyring_id(extension_id: &str, key: &str) -> String {
        format!("ext-secret::{extension_id}::{key}")
    }

    /// Store an extension secret (e.g. an LLM API key) securely in the system keyring.
    pub fn set_extension_secret(
        &self,
        extension_id: &str,
        key: &str,
        secret: &str,
    ) -> Result<(), String> {
        let keyring_id = Self::secret_keyring_id(extension_id, key);
        if secret.trim().is_empty() {
            let _ = super::credential_service::CredentialService::remove_password(&keyring_id);
        } else {
            super::credential_service::CredentialService::set_password(&keyring_id, secret)?;
        }
        notify_extensions_changed();
        Ok(())
    }

    /// Retrieve an extension secret from the system keyring.
    pub fn get_extension_secret(&self, extension_id: &str, key: &str) -> Option<String> {
        let keyring_id = Self::secret_keyring_id(extension_id, key);
        super::credential_service::CredentialService::fetch_password(&keyring_id)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    }

    /// Resolve a setting value (handling `Secret`, `Bool`, and `String` with default fallback).
    pub fn get_effective_setting_value(
        &self,
        ext: &InstalledExtension,
        key: &str,
    ) -> Option<String> {
        let schema = ext
            .manifest
            .contributes
            .settings
            .iter()
            .find(|s| s.key == key)?;
        if schema.kind == ExtensionSettingKind::Secret {
            return self.get_extension_secret(&ext.manifest.id, key);
        }
        let stored = self.extension_settings_values(&ext.manifest.id);
        Some(
            stored
                .get(key)
                .cloned()
                .unwrap_or_else(|| schema.default_value.clone()),
        )
    }

    pub fn effective_view_placement(
        &self,
        extension_id: &str,
        view: &ExtensionViewContribution,
    ) -> ExtensionViewPlacement {
        let state = self.load_state();
        let composite = format!("{extension_id}:{}", view.id);
        state
            .view_placement_overrides
            .get(&composite)
            .copied()
            .unwrap_or(view.placement)
    }

    pub fn set_view_placement_override(
        &self,
        extension_id: &str,
        view_id: &str,
        placement: ExtensionViewPlacement,
    ) -> io::Result<()> {
        let mut state = self.load_state();
        state
            .view_placement_overrides
            .insert(format!("{extension_id}:{view_id}"), placement);
        self.save_state(&state)?;
        notify_extensions_changed();
        Ok(())
    }

    fn resolve_setting_placeholders(
        &self,
        ext: &InstalledExtension,
        values: &BTreeMap<String, String>,
        raw: &str,
    ) -> String {
        if !raw.contains("{{setting:") {
            return raw.to_string();
        }
        let mut out = raw.to_string();
        for schema in &ext.manifest.contributes.settings {
            let token = format!("{{{{setting:{}}}}}", schema.key);
            if out.contains(&token) {
                let effective = values
                    .get(&schema.key)
                    .filter(|v| !v.is_empty())
                    .map(String::as_str)
                    .unwrap_or(schema.default_value.as_str());
                out = out.replace(&token, effective);
            }
        }
        out
    }

    pub fn list_all(&self) -> Vec<InstalledExtension> {
        let state = self.load_state();
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        for manifest in bundled_extensions() {
            let enabled = state
                .enabled_overrides
                .get(&manifest.id)
                .copied()
                .unwrap_or(manifest.enabled_by_default);
            seen.insert(manifest.id.clone());
            out.push(InstalledExtension {
                manifest,
                root_dir: None,
                is_bundled: true,
                enabled,
            });
        }

        if let Ok(entries) = fs::read_dir(&self.ext_dir) {
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for dir in dirs {
                let manifest_path = dir.join("extension.json");
                let Ok(raw) = fs::read_to_string(&manifest_path) else {
                    continue;
                };
                let Ok(manifest) = serde_json::from_str::<ExtensionManifest>(&raw) else {
                    tracing::warn!(
                        path = %manifest_path.display(),
                        "Skipping invalid extension manifest"
                    );
                    continue;
                };
                if manifest.id.trim().is_empty() || !seen.insert(manifest.id.clone()) {
                    continue;
                }
                let enabled = state
                    .enabled_overrides
                    .get(&manifest.id)
                    .copied()
                    .unwrap_or(manifest.enabled_by_default);
                out.push(InstalledExtension {
                    manifest,
                    root_dir: Some(dir),
                    is_bundled: false,
                    enabled,
                });
            }
        }

        out
    }

    pub fn list_enabled(&self) -> Vec<InstalledExtension> {
        self.list_all().into_iter().filter(|e| e.enabled).collect()
    }

    pub fn is_enabled(&self, extension_id: &str) -> bool {
        self.list_all()
            .into_iter()
            .find(|e| e.manifest.id == extension_id)
            .is_some_and(|e| e.enabled)
    }

    pub fn set_enabled(&self, extension_id: &str, enabled: bool) -> io::Result<()> {
        let mut state = self.load_state();
        state
            .enabled_overrides
            .insert(extension_id.to_string(), enabled);
        self.save_state(&state)?;
        notify_extensions_changed();
        Ok(())
    }

    pub fn enabled_header_actions(
        &self,
        surface: ExtensionSurface,
    ) -> Vec<(InstalledExtension, ExtensionHeaderActionContribution)> {
        let mut actions = Vec::new();
        for ext in self.list_enabled() {
            for btn in &ext.manifest.contributes.header_actions {
                if btn.surface == surface {
                    actions.push((ext.clone(), btn.clone()));
                }
            }
        }
        actions
    }

    pub fn enabled_sidebar_items(
        &self,
    ) -> Vec<(InstalledExtension, ExtensionSidebarContribution)> {
        let mut items = Vec::new();
        for ext in self.list_enabled() {
            for item in &ext.manifest.contributes.sidebar_items {
                items.push((ext.clone(), item.clone()));
            }
        }
        items
    }

    pub fn enabled_commands(&self) -> Vec<(InstalledExtension, ExtensionCommandContribution)> {
        let mut cmds = Vec::new();
        for ext in self.list_enabled() {
            for cmd in &ext.manifest.contributes.commands {
                cmds.push((ext.clone(), cmd.clone()));
            }
        }
        cmds
    }

    pub fn enabled_message_actions(
        &self,
    ) -> Vec<(InstalledExtension, ExtensionMessageActionContribution)> {
        let mut actions = Vec::new();
        for ext in self.list_enabled() {
            for act in &ext.manifest.contributes.message_actions {
                actions.push((ext.clone(), act.clone()));
            }
        }
        actions
    }

    pub fn enabled_views(&self) -> Vec<(InstalledExtension, ExtensionViewContribution)> {
        let state = self.load_state();
        let mut views = Vec::new();
        for ext in self.list_enabled() {
            let values = state
                .settings_values
                .get(&ext.manifest.id)
                .cloned()
                .unwrap_or_default();
            for view in &ext.manifest.contributes.views {
                let mut resolved = view.clone();
                resolved.title = self.resolve_setting_placeholders(&ext, &values, &view.title);
                resolved.url = self.resolve_setting_placeholders(&ext, &values, &view.url);
                if !resolved.url.trim().is_empty()
                    && !ext.has_permission_kind(ExtensionPermission::Network)
                {
                    continue;
                }
                let composite = format!("{}:{}", ext.manifest.id, view.id);
                if let Some(override_placement) = state.view_placement_overrides.get(&composite) {
                    resolved.placement = *override_placement;
                }
                views.push((ext.clone(), resolved));
            }
        }
        views
    }

    pub fn enabled_stylesheets(&self, target: StylesheetTarget) -> String {
        let mut combined = String::new();
        for ext in self.list_enabled() {
            if !ext.has_permission_kind(ExtensionPermission::UiCustomize) {
                continue;
            }
            for sheet in &ext.manifest.contributes.stylesheets {
                if sheet.target != target {
                    continue;
                }
                if !sheet.path.trim().is_empty() && !sheet.path.contains("..") {
                    if let Some(root) = &ext.root_dir {
                        let candidate = root.join(sheet.path.trim());
                        if let Ok(content) = fs::read_to_string(&candidate) {
                            if !content.trim().is_empty() {
                                combined.push_str(&format!(
                                    "\n/* Extension: {} ({}) */\n{}\n",
                                    ext.manifest.id, sheet.path, content
                                ));
                            }
                        }
                    }
                }
                if !sheet.css.trim().is_empty() {
                    combined.push_str(&format!(
                        "\n/* Extension inline: {} */\n{}\n",
                        ext.manifest.id, sheet.css
                    ));
                }
            }
        }
        combined
    }

    pub fn enabled_app_css(&self) -> String {
        self.enabled_stylesheets(StylesheetTarget::App)
    }

    pub fn enabled_mail_viewer_css(&self) -> String {
        self.enabled_stylesheets(StylesheetTarget::MailViewer)
    }

    pub fn enabled_message_transformers(
        &self,
        target: TransformerTarget,
    ) -> Vec<(InstalledExtension, ExtensionMessageTransformer)> {
        let state = self.load_state();
        let mut rules = Vec::new();
        for ext in self.list_enabled() {
            if !ext.has_permission_kind(ExtensionPermission::ModifyMailDisplay) {
                continue;
            }
            let has_read = match target {
                TransformerTarget::BodyHtml => {
                    ext.has_permission_kind(ExtensionPermission::ReadMailBody)
                }
                TransformerTarget::Subject | TransformerTarget::ListBadge => {
                    ext.has_permission_kind(ExtensionPermission::ReadMailHeaders)
                }
            };
            if !has_read {
                continue;
            }
            let values = state
                .settings_values
                .get(&ext.manifest.id)
                .cloned()
                .unwrap_or_default();
            for rule in &ext.manifest.contributes.message_transformers {
                if rule.target == target {
                    let mut resolved = rule.clone();
                    resolved.pattern =
                        self.resolve_setting_placeholders(&ext, &values, &rule.pattern);
                    resolved.replacement =
                        self.resolve_setting_placeholders(&ext, &values, &rule.replacement);
                    rules.push((ext.clone(), resolved));
                }
            }
        }
        rules
    }

    /// Apply enabled regex transformations targeting the email HTML body.
    /// Replacements only run on text nodes outside `<...>` HTML tags so markup stays intact.
    pub fn apply_message_html_transformers(&self, html: &str) -> String {
        let rules = self.enabled_message_transformers(TransformerTarget::BodyHtml);
        if rules.is_empty() {
            return html.to_string();
        }
        let mut current = html.to_string();
        for (_, rule) in rules {
            let Ok(re) = Regex::new(&rule.pattern) else {
                continue;
            };
            current = replace_outside_html_tags(&current, &re, &rule.replacement);
        }
        current
    }

    /// Apply enabled regex transformations targeting message subjects.
    pub fn apply_message_subject_transformers(&self, subject: &str) -> String {
        let rules = self.enabled_message_transformers(TransformerTarget::Subject);
        if rules.is_empty() {
            return subject.to_string();
        }
        let mut current = subject.to_string();
        for (_, rule) in rules {
            let Ok(re) = Regex::new(&rule.pattern) else {
                continue;
            };
            current = re.replace_all(&current, rule.replacement.as_str()).into_owned();
        }
        current
    }

    /// Collect custom list badges `(label, optional_hex_color)` matched by `ListBadge` regex rules.
    pub fn message_list_badges(&self, subject: &str, sender: &str) -> Vec<(String, String)> {
        let rules = self.enabled_message_transformers(TransformerTarget::ListBadge);
        if rules.is_empty() {
            return Vec::new();
        }
        let haystack = format!("{subject}\n{sender}");
        let mut badges = Vec::new();
        for (_, rule) in rules {
            let Ok(re) = Regex::new(&rule.pattern) else {
                continue;
            };
            if let Some(caps) = re.captures(&haystack) {
                let mut label = String::new();
                caps.expand(&rule.replacement, &mut label);
                let label = label.trim().to_string();
                if !label.is_empty() {
                    badges.push((label, rule.badge_color.trim().to_string()));
                }
            }
        }
        badges
    }

    pub fn enabled_attachment_renderers(
        &self,
    ) -> Vec<(InstalledExtension, ExtensionAttachmentRenderer)> {
        let mut renderers = Vec::new();
        for ext in self.list_enabled() {
            if !ext.has_permission_kind(ExtensionPermission::ReadMailAttachments) {
                continue;
            }
            for r in &ext.manifest.contributes.attachment_renderers {
                let exec = r.exec.trim();
                if !exec.is_empty()
                    && !exec.starts_with("builtin:")
                    && !ext.has_permission_kind(ExtensionPermission::Shell)
                {
                    continue;
                }
                renderers.push((ext.clone(), r.clone()));
            }
        }
        renderers
    }

    /// Render an attachment inline as sanitized HTML if an enabled extension provides a matching renderer.
    /// Returns `(renderer_title, sanitized_html_fragment)`.
    pub fn render_inline_attachment(
        &self,
        filename: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Option<(String, String)> {
        let lower_ext = Path::new(filename)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let lower_mime = mime_type.trim().to_ascii_lowercase();

        for (ext, renderer) in self.enabled_attachment_renderers() {
            let ext_match = !lower_ext.is_empty()
                && renderer
                    .extensions
                    .iter()
                    .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&lower_ext));
            let mime_match = !lower_mime.is_empty()
                && renderer
                    .mime_types
                    .iter()
                    .any(|m| m.trim().eq_ignore_ascii_case(&lower_mime));
            if !ext_match && !mime_match {
                continue;
            }
            if let Some(builtin) = renderer.exec.strip_prefix("builtin:") {
                if let Some(html) = render_builtin_attachment_html(builtin, filename, content) {
                    return Some((renderer.title, html));
                }
            } else if !renderer.exec.trim().is_empty() && !renderer.exec.contains("..") {
                if let Some(root_dir) = &ext.root_dir {
                    if let Some(html) =
                        run_external_attachment_renderer(root_dir, &renderer.exec, filename, mime_type, content)
                    {
                        return Some((renderer.title, html));
                    }
                }
            }
        }
        None
    }

    /// Collect active translation key->value overrides from enabled language-pack extensions
    /// matching `active_locale` (e.g. `"de_DE"`, `"ja"`, or wildcard `"*"`).
    pub fn active_locale_overrides(&self, active_locale: &str) -> HashMap<String, String> {
        let mut overrides = HashMap::new();
        let active_norm = active_locale
            .split('.')
            .next()
            .unwrap_or(active_locale)
            .replace('-', "_")
            .to_ascii_lowercase();
        let active_short = active_norm.split('_').next().unwrap_or(&active_norm);

        for ext in self.list_enabled() {
            if !ext.has_permission_kind(ExtensionPermission::UiCustomize) {
                continue;
            }
            let Some(root_dir) = &ext.root_dir else {
                continue;
            };
            for loc in &ext.manifest.contributes.locales {
                if loc.json_file.trim().is_empty() || loc.json_file.contains("..") {
                    continue;
                }
                let code_norm = loc.code.trim().replace('-', "_").to_ascii_lowercase();
                let matches = code_norm == "*"
                    || (!active_norm.is_empty()
                        && (code_norm == active_norm
                            || code_norm == active_short
                            || code_norm.split('_').next().unwrap_or("") == active_short));
                if !matches {
                    continue;
                }
                let path = root_dir.join(loc.json_file.trim());
                let Ok(raw) = fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
                    for (k, v) in map {
                        if !k.starts_with('_') && !v.is_empty() {
                            overrides.insert(k, v);
                        }
                    }
                }
            }
        }
        overrides
    }

    /// Parse and validate `extension.json` from a local folder without installing it yet,
    /// so the UI can display all audited permissions for user review before copying files.
    pub fn inspect_directory_manifest(source_dir: &Path) -> Result<ExtensionManifest, String> {
        let manifest_file = source_dir.join("extension.json");
        let raw = fs::read_to_string(&manifest_file)
            .map_err(|e| format!("Could not read extension.json: {e}"))?;
        let manifest: ExtensionManifest = serde_json::from_str(&raw)
            .map_err(|e| format!("Invalid extension.json format: {e}"))?;
        validate_manifest(&manifest)?;
        Ok(manifest)
    }

    /// Install a user extension from a source folder containing `extension.json`.
    pub fn install_from_directory(&self, source_dir: &Path) -> Result<ExtensionManifest, String> {
        let manifest = Self::inspect_directory_manifest(source_dir)?;

        let safe_folder_name = sanitize_extension_id(&manifest.id);
        let dest_dir = self.ext_dir.join(&safe_folder_name);
        fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("Could not create extension directory: {e}"))?;
        copy_dir_flat_or_nested(source_dir, &dest_dir)
            .map_err(|e| format!("Could not copy extension files: {e}"))?;

        let mut state = self.load_state();
        state.enabled_overrides.insert(manifest.id.clone(), true);
        let _ = self.save_state(&state);
        notify_extensions_changed();
        Ok(manifest)
    }

    pub fn uninstall(&self, extension_id: &str) -> Result<(), String> {
        let all = self.list_all();
        let Some(found) = all.into_iter().find(|e| e.manifest.id == extension_id) else {
            return Err("Extension not found".to_string());
        };
        if found.is_bundled {
            return Err("Built-in add-ons cannot be uninstalled; disable them instead".to_string());
        }
        if let Some(dir) = found.root_dir {
            if dir.starts_with(&self.ext_dir) {
                fs::remove_dir_all(&dir)
                    .map_err(|e| format!("Failed to remove extension folder: {e}"))?;
            }
        }
        let mut state = self.load_state();
        state.enabled_overrides.remove(extension_id);
        let _ = self.save_state(&state);
        notify_extensions_changed();
        Ok(())
    }

    /// Create a ready-to-edit starter extension in `<extensions_dir>/<slug>/`
    /// so developers and power users can build custom mods immediately.
    pub fn create_starter_extension(&self, display_name: &str) -> Result<PathBuf, String> {
        let clean_name = display_name.trim();
        if clean_name.is_empty() {
            return Err("Extension name must not be empty".to_string());
        }
        let slug = sanitize_extension_id(&clean_name.to_ascii_lowercase().replace(' ', "-"));
        let ext_id = format!("custom.{slug}");
        let folder = self.ext_dir.join(&ext_id);
        if folder.exists() {
            return Err(format!("Extension '{}' already exists", ext_id));
        }
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;

        let script_name = "run.py";
        let manifest = ExtensionManifest {
            id: ext_id.clone(),
            name: clean_name.to_string(),
            version: "0.1.0".to_string(),
            author: "Local User".to_string(),
            description: "Custom Meridian extension created from starter template.".to_string(),
            icon_name: "application-x-addon-symbolic".to_string(),
            enabled_by_default: true,
            permissions: vec!["shell".to_string(), "mail.headers".to_string()],
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: format!("{ext_id}.run"),
                    title: format!("{clean_name}: Run Action"),
                    category: "Custom".to_string(),
                    shortcut: String::new(),
                    exec: script_name.to_string(),
                    args: Vec::new(),
                }],
                header_actions: vec![ExtensionHeaderActionContribution {
                    id: format!("{ext_id}.btn"),
                    command: format!("{ext_id}.run"),
                    icon_name: "media-playback-start-symbolic".to_string(),
                    tooltip: clean_name.to_string(),
                    surface: ExtensionSurface::Mail,
                }],
                message_actions: vec![ExtensionMessageActionContribution {
                    id: format!("{ext_id}.msg"),
                    command: format!("{ext_id}.run"),
                    label: clean_name.to_string(),
                }],
                ..ExtensionContributions::default()
            },
        };

        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        fs::write(folder.join("extension.json"), manifest_json).map_err(|e| e.to_string())?;

        let script = r#"#!/usr/bin/env python3
import json, sys

ctx = json.load(sys.stdin)
subject = ctx.get("message_subject") or ctx.get("composer_subject") or "no selection"
response = {
    "toast": f"Processed: {subject}"
}
print(json.dumps(response))
"#;
        let script_path = folder.join(script_name);
        fs::write(&script_path, script).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&script_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(&script_path, perms);
            }
        }

        let mut state = self.load_state();
        state.enabled_overrides.insert(ext_id, true);
        let _ = self.save_state(&state);
        let _ = ensure_extension_guide_written(&self.ext_dir);
        notify_extensions_changed();
        Ok(folder)
    }

    /// Execute an extension command outside the GTK main thread.
    pub fn execute_command(
        &self,
        command_id: &str,
        context: &ExtensionExecutionContext,
    ) -> Result<ExtensionHostResponse, String> {
        let Some((ext, cmd)) = self
            .enabled_commands()
            .into_iter()
            .find(|(_, c)| c.id == command_id)
        else {
            return Err(format!("Extension command '{command_id}' is not enabled"));
        };

        if let Some(builtin) = cmd.exec.strip_prefix("builtin:") {
            return execute_builtin_command(builtin, context);
        }

        if !ext.has_permission_kind(ExtensionPermission::Shell) {
            return Err(
                "Permission denied: extension must declare 'shell' to run local scripts"
                    .to_string(),
            );
        }

        if cmd.exec.trim().is_empty() {
            return Err("Command has no executable configured".to_string());
        }

        let Some(root_dir) = ext.root_dir.clone() else {
            return Err("External command requires an extension directory".to_string());
        };
        let candidate = root_dir.join(&cmd.exec);
        let program = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(&cmd.exec)
        };

        // Strictly redact context fields according to the extension's declared permissions.
        let scoped_context = redact_execution_context(&ext, context);

        let input_bytes = serde_json::to_vec(&scoped_context).map_err(|e| e.to_string())?;
        let mut child = Command::new(&program)
            .args(&cmd.args)
            .current_dir(&root_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start '{}': {e}", program.display()))?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input_bytes);
        }

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut stdout_buf = Vec::new();
                    if let Some(mut out) = child.stdout.take() {
                        let _ = (&mut out)
                            .take(MAX_SCRIPT_OUTPUT_BYTES as u64)
                            .read_to_end(&mut stdout_buf);
                    }
                    let mut stderr_buf = Vec::new();
                    if let Some(mut err) = child.stderr.take() {
                        let _ = (&mut err).take(16 * 1024).read_to_end(&mut stderr_buf);
                    }
                    if !status.success() {
                        let err_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();
                        return Err(if err_text.is_empty() {
                            format!("Extension exited with status {status}")
                        } else {
                            err_text
                        });
                    }
                    let trimmed = String::from_utf8_lossy(&stdout_buf);
                    let trimmed = trimmed.trim();
                    if trimmed.is_empty() {
                        return Ok(ExtensionHostResponse::default());
                    }
                    if let Ok(parsed) = serde_json::from_str::<ExtensionHostResponse>(trimmed) {
                        return Ok(sanitize_host_response(&ext, parsed));
                    }
                    return Ok(ExtensionHostResponse {
                        toast: None,
                        output_title: Some(cmd.title),
                        output_body: Some(trimmed.to_string()),
                        ..ExtensionHostResponse::default()
                    });
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("Extension command timed out after 15 seconds".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(format!("Failed waiting for extension process: {e}")),
            }
        }
    }
}

fn execute_builtin_command(
    builtin_id: &str,
    _context: &ExtensionExecutionContext,
) -> Result<ExtensionHostResponse, String> {
    match builtin_id {
        "mail-merge.insert" => Ok(ExtensionHostResponse {
            insert_composer_html: Some("{{first_name}}".to_string()),
            toast: Some("Inserted {{first_name}} placeholder — expands per recipient on send".to_string()),
            ..ExtensionHostResponse::default()
        }),
        "activity.open" => {
            let all = super::log_buffer::recent_logs();
            let start = all.len().saturating_sub(80);
            let entries = &all[start..];
            let body = if entries.is_empty() {
                "No recent background operations recorded in this session.".to_string()
            } else {
                entries.join("\n")
            };
            Ok(ExtensionHostResponse {
                output_title: Some("Activity Monitor".to_string()),
                output_body: Some(body),
                ..ExtensionHostResponse::default()
            })
        }
        "rss.manage" => Ok(ExtensionHostResponse {
            toast: Some("Select Feeds in the sidebar to read and manage subscriptions".to_string()),
            ..ExtensionHostResponse::default()
        }),
        other => Err(format!("Unknown built-in extension command '{other}'")),
    }
}

/// Expand Mail Merge variables (`{{name}}`, `{{first_name}}`, `{{email}}`) for a single recipient.
pub fn expand_mail_merge_for_recipient(template: &str, recipient_raw: &str) -> String {
    let (display_name, email) = split_recipient_display_and_email(recipient_raw);
    let effective_name = if display_name.is_empty() {
        email
            .split('@')
            .next()
            .unwrap_or(&email)
            .replace(['.', '_', '-'], " ")
    } else {
        display_name.clone()
    };
    let first_name = effective_name
        .split_whitespace()
        .next()
        .unwrap_or(&effective_name);

    template
        .replace("{{name}}", &effective_name)
        .replace("{{Name}}", &effective_name)
        .replace("{{first_name}}", first_name)
        .replace("{{FirstName}}", first_name)
        .replace("{{email}}", &email)
        .replace("{{Email}}", &email)
}

pub fn has_mail_merge_placeholders(text: &str) -> bool {
    ["{{name}}", "{{Name}}", "{{first_name}}", "{{FirstName}}", "{{email}}", "{{Email}}"]
        .iter()
        .any(|token| text.contains(token))
}

fn split_recipient_display_and_email(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    if let (Some(lt), Some(gt)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
        if lt < gt {
            let name = trimmed[..lt].trim().trim_matches('"').trim().to_string();
            let addr = trimmed[lt + 1..gt].trim().to_string();
            return (name, addr);
        }
    }
    (String::new(), trimmed.to_string())
}

/// Redact execution context fields according to an extension's declared permissions.
pub fn redact_execution_context(
    ext: &InstalledExtension,
    context: &ExtensionExecutionContext,
) -> ExtensionExecutionContext {
    let mut scoped = context.clone();
    let can_read_body = ext.has_permission_kind(ExtensionPermission::ReadMailBody);
    let can_read_headers =
        can_read_body || ext.has_permission_kind(ExtensionPermission::ReadMailHeaders);
    if !can_read_body {
        scoped.message_body_text = None;
    }
    if !can_read_headers {
        scoped.message_id = None;
        scoped.message_subject = None;
        scoped.message_sender = None;
        scoped.message_recipients = None;
        scoped.message_date = None;
    }
    if !ext.has_permission_kind(ExtensionPermission::ComposeMail) {
        scoped.composer_to = None;
        scoped.composer_subject = None;
        scoped.composer_body_html = None;
    }
    scoped
}

/// Strip unauthorized host mutations from an external command's JSON stdout response.
pub fn sanitize_host_response(
    ext: &InstalledExtension,
    mut response: ExtensionHostResponse,
) -> ExtensionHostResponse {
    if !ext.has_permission_kind(ExtensionPermission::Network) {
        response.open_url = None;
    }
    if !ext.has_permission_kind(ExtensionPermission::ComposeMail) {
        response.insert_composer_html = None;
        response.replace_composer_html = None;
        response.replace_composer_subject = None;
    }
    if !ext.has_permission_kind(ExtensionPermission::WriteTasks) {
        response.create_task_title = None;
    }
    if !ext.has_permission_kind(ExtensionPermission::ModifyMailDisplay) {
        response.add_label = None;
    }
    response
}

fn validate_manifest(manifest: &ExtensionManifest) -> Result<(), String> {
    if manifest.id.trim().is_empty() || manifest.id.len() > 128 {
        return Err("Extension ID must be 1..128 characters".to_string());
    }
    if !manifest
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err("Extension ID may only contain ASCII letters, digits, '.', '-', '_'".to_string());
    }
    if manifest.name.trim().is_empty() {
        return Err("Extension name must not be empty".to_string());
    }
    for cmd in &manifest.contributes.commands {
        if cmd.exec.contains("..") {
            return Err("Command executable path must not traverse parent directories".to_string());
        }
    }
    for view in &manifest.contributes.views {
        if view.html_file.contains("..") {
            return Err("View html_file path must not traverse parent directories".to_string());
        }
    }
    for sheet in &manifest.contributes.stylesheets {
        if sheet.path.contains("..") {
            return Err("Stylesheet path must not traverse parent directories".to_string());
        }
    }
    for renderer in &manifest.contributes.attachment_renderers {
        if renderer.exec.contains("..") {
            return Err("Attachment renderer path must not traverse parent directories".to_string());
        }
    }
    for locale in &manifest.contributes.locales {
        if locale.json_file.contains("..") {
            return Err("Locale JSON path must not traverse parent directories".to_string());
        }
    }
    Ok(())
}

/// Apply a regex replacement only to text outside HTML tags (`<...>`), avoiding corruption of attributes/URLs.
fn replace_outside_html_tags(html: &str, re: &Regex, replacement: &str) -> String {
    let mut out = String::with_capacity(html.len() + 64);
    let mut in_tag = false;
    let mut in_style_or_script = false;
    let mut seg_start = 0usize;
    let bytes = html.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !in_tag && b == b'<' {
            let text_seg = &html[seg_start..i];
            if in_style_or_script || text_seg.is_empty() {
                out.push_str(text_seg);
            } else {
                out.push_str(&re.replace_all(text_seg, replacement));
            }
            seg_start = i;
            in_tag = true;
        } else if in_tag && b == b'>' {
            let tag_seg = &html[seg_start..=i];
            let lower = tag_seg.to_ascii_lowercase();
            if lower.starts_with("<style") || lower.starts_with("<script") {
                in_style_or_script = true;
            } else if lower.starts_with("</style") || lower.starts_with("</script") {
                in_style_or_script = false;
            }
            out.push_str(tag_seg);
            seg_start = i + 1;
            in_tag = false;
        }
        i += 1;
    }
    if seg_start < html.len() {
        let tail = &html[seg_start..];
        if in_tag || in_style_or_script {
            out.push_str(tail);
        } else {
            out.push_str(&re.replace_all(tail, replacement));
        }
    }
    out
}

fn escape_html_inline(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_builtin_attachment_html(builtin: &str, filename: &str, content: &[u8]) -> Option<String> {
    const MAX_PREVIEW_BYTES: usize = 128 * 1024;
    let slice = if content.len() > MAX_PREVIEW_BYTES {
        &content[..MAX_PREVIEW_BYTES]
    } else {
        content
    };
    let text = String::from_utf8_lossy(slice);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    match builtin {
        "markdown" => {
            let mut html = String::from("<div class=\"meridian-ext-md\">");
            let mut in_code = false;
            let mut in_list = false;
            for line in trimmed.lines() {
                if line.trim_start().starts_with("```") {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    if in_code {
                        html.push_str("</code></pre>");
                        in_code = false;
                    } else {
                        html.push_str("<pre><code>");
                        in_code = true;
                    }
                    continue;
                }
                if in_code {
                    html.push_str(&escape_html_inline(line));
                    html.push('\n');
                    continue;
                }
                let t = line.trim();
                if t.is_empty() {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    continue;
                }
                if let Some(rest) = t.strip_prefix("### ") {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    html.push_str(&format!("<h4>{}</h4>", format_inline_md(rest)));
                } else if let Some(rest) = t.strip_prefix("## ") {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    html.push_str(&format!("<h3>{}</h3>", format_inline_md(rest)));
                } else if let Some(rest) = t.strip_prefix("# ") {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    html.push_str(&format!("<h2>{}</h2>", format_inline_md(rest)));
                } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                    if !in_list {
                        html.push_str("<ul>");
                        in_list = true;
                    }
                    html.push_str(&format!("<li>{}</li>", format_inline_md(rest)));
                } else if t == "---" || t == "***" {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    html.push_str("<hr/>");
                } else {
                    if in_list {
                        html.push_str("</ul>");
                        in_list = false;
                    }
                    html.push_str(&format!("<p>{}</p>", format_inline_md(t)));
                }
            }
            if in_list {
                html.push_str("</ul>");
            }
            if in_code {
                html.push_str("</code></pre>");
            }
            html.push_str("</div>");
            Some(super::html_sanitizer::sanitize_html(&html))
        }
        "csv" => {
            let first_line = trimmed.lines().next().unwrap_or("");
            let sep = if first_line.matches(';').count() > first_line.matches(',').count() {
                ';'
            } else {
                ','
            };
            let mut html = String::from(
                "<div class=\"meridian-ext-csv\" style=\"overflow-x:auto;\"><table style=\"border-collapse:collapse;width:100%;font-size:0.9em;\">",
            );
            for (row_idx, line) in trimmed.lines().take(200).enumerate() {
                let cells = split_csv_row(line, sep);
                html.push_str("<tr>");
                let cell_tag = if row_idx == 0 { "th" } else { "td" };
                let style = if row_idx == 0 {
                    "border:1px solid rgba(128,128,128,0.28);padding:4px 8px;text-align:left;background:rgba(128,128,128,0.12);"
                } else {
                    "border:1px solid rgba(128,128,128,0.22);padding:4px 8px;"
                };
                for cell in cells.into_iter().take(30) {
                    html.push_str(&format!(
                        "<{cell_tag} style=\"{style}\">{}</{cell_tag}>",
                        escape_html_inline(cell.trim())
                    ));
                }
                html.push_str("</tr>");
            }
            html.push_str("</table></div>");
            Some(super::html_sanitizer::sanitize_html(&html))
        }
        "json" => {
            let pretty = serde_json::from_str::<serde_json::Value>(trimmed)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| trimmed.to_string());
            let html = format!(
                "<pre class=\"meridian-ext-json\" style=\"overflow-x:auto;padding:10px;border-radius:8px;background:rgba(128,128,128,0.1);font-size:0.86em;\"><code>{}</code></pre>",
                escape_html_inline(&pretty)
            );
            Some(super::html_sanitizer::sanitize_html(&html))
        }
        "e-invoice" => {
            let is_invoice = trimmed.contains("CrossIndustryInvoice")
                || trimmed.contains("Invoice")
                || trimmed.contains("PayableAmount")
                || trimmed.contains("GrandTotalAmount");
            if is_invoice {
                let inv_id = extract_xml_tag_any_ns(trimmed, "ID").unwrap_or_else(|| filename.to_string());
                let currency = extract_xml_tag_any_ns(trimmed, "InvoiceCurrencyCode")
                    .or_else(|| extract_xml_tag_any_ns(trimmed, "DocumentCurrencyCode"))
                    .unwrap_or_default();
                let total = extract_xml_tag_any_ns(trimmed, "GrandTotalAmount")
                    .or_else(|| extract_xml_tag_any_ns(trimmed, "DuePayableAmount"))
                    .or_else(|| extract_xml_tag_any_ns(trimmed, "PayableAmount"))
                    .unwrap_or_else(|| "—".to_string());
                let issue_date = extract_xml_tag_any_ns(trimmed, "IssueDate")
                    .or_else(|| extract_xml_tag_any_ns(trimmed, "DateTimeString"))
                    .unwrap_or_else(|| "—".to_string());
                let iban = extract_xml_tag_any_ns(trimmed, "IBANID").unwrap_or_default();
                let mut rows = format!(
                    "<tr><th style=\"text-align:left;padding:3px 10px 3px 0;opacity:0.75;\">Invoice</th><td>{}</td></tr>\
                     <tr><th style=\"text-align:left;padding:3px 10px 3px 0;opacity:0.75;\">Date</th><td>{}</td></tr>\
                     <tr><th style=\"text-align:left;padding:3px 10px 3px 0;opacity:0.75;\">Total</th><td><strong>{} {}</strong></td></tr>",
                    escape_html_inline(&inv_id),
                    escape_html_inline(&issue_date),
                    escape_html_inline(&total),
                    escape_html_inline(&currency),
                );
                if !iban.is_empty() {
                    rows.push_str(&format!(
                        "<tr><th style=\"text-align:left;padding:3px 10px 3px 0;opacity:0.75;\">IBAN</th><td><code>{}</code></td></tr>",
                        escape_html_inline(&iban)
                    ));
                }
                let html = format!(
                    "<div class=\"meridian-ext-invoice\"><table style=\"font-size:0.92em;\">{rows}</table></div>"
                );
                Some(super::html_sanitizer::sanitize_html(&html))
            } else {
                let html = format!(
                    "<pre class=\"meridian-ext-xml\" style=\"overflow-x:auto;padding:10px;border-radius:8px;background:rgba(128,128,128,0.1);font-size:0.84em;\"><code>{}</code></pre>",
                    escape_html_inline(trimmed)
                );
                Some(super::html_sanitizer::sanitize_html(&html))
            }
        }
        _ => None,
    }
}

fn format_inline_md(raw: &str) -> String {
    let mut s = escape_html_inline(raw);
    if let Ok(re_code) = Regex::new(r"`([^`]+)`") {
        s = re_code.replace_all(&s, "<code>$1</code>").into_owned();
    }
    if let Ok(re_bold) = Regex::new(r"\*\*([^*]+)\*\*") {
        s = re_bold.replace_all(&s, "<strong>$1</strong>").into_owned();
    }
    if let Ok(re_italic) = Regex::new(r"\*([^*]+)\*") {
        s = re_italic.replace_all(&s, "<em>$1</em>").into_owned();
    }
    s
}

fn split_csv_row(line: &str, sep: char) -> Vec<String> {
    let mut cols = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == sep && !in_quotes {
            cols.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    cols.push(cur);
    cols
}

fn extract_xml_tag_any_ns(xml: &str, local_name: &str) -> Option<String> {
    let pat = format!(r"<(?:[A-Za-z0-9_-]+:)?{local_name}\b[^>]*>([^<]+)</(?:[A-Za-z0-9_-]+:)?{local_name}>");
    let re = Regex::new(&pat).ok()?;
    let caps = re.captures(xml)?;
    let val = caps.get(1)?.as_str().trim();
    if val.is_empty() {
        None
    } else {
        Some(decode_xml_entities(val))
    }
}

fn run_external_attachment_renderer(
    root_dir: &Path,
    exec: &str,
    filename: &str,
    mime_type: &str,
    content: &[u8],
) -> Option<String> {
    let candidate = root_dir.join(exec);
    let program = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(exec)
    };
    let payload = serde_json::json!({
        "filename": filename,
        "mime_type": mime_type,
        "content_utf8": String::from_utf8_lossy(&content[..content.len().min(128 * 1024)]),
    });
    let input_bytes = serde_json::to_vec(&payload).ok()?;
    let mut child = Command::new(&program)
        .current_dir(root_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input_bytes);
    }
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = Vec::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = (&mut stdout).take(256 * 1024).read_to_end(&mut out);
                }
                let html = String::from_utf8_lossy(&out).trim().to_string();
                if html.is_empty() {
                    return None;
                }
                return Some(super::html_sanitizer::sanitize_html(&html));
            }
            Ok(None) => {
                if start.elapsed() >= Duration::from_secs(5) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

// ─── Extension Discover Catalog (VS Code / Obsidian-style Extension Browser) ───

pub const DEFAULT_EXTENSION_REGISTRY_URL: &str =
    "https://meridian.martingawron.com/extensions/registry.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogExtensionEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub category: String,
    pub description: String,
    #[serde(default = "default_extension_icon")]
    pub icon_name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub homepage_url: String,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub builtin_template: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RemoteCatalogPayload {
    #[serde(default)]
    extensions: Vec<CatalogExtensionEntry>,
}

#[derive(Debug, Clone)]
pub struct ExtensionCatalogService {
    registry_url: String,
}

impl CatalogExtensionEntry {
    /// Return audited canonical permissions for this catalog item (derived from its template
    /// contributions when bundled, combined with its declared permissions).
    pub fn audited_permissions(&self) -> Vec<ExtensionPermission> {
        if let Some(tpl) = self.builtin_template.as_deref() {
            if let Ok((manifest, _)) = build_catalog_template(tpl, self) {
                return manifest.audited_permissions();
            }
        }
        ExtensionPermission::normalize_list(&self.permissions)
    }
}

impl Default for ExtensionCatalogService {
    fn default() -> Self {
        let url = std::env::var("MERIDIAN_EXTENSION_REGISTRY_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_EXTENSION_REGISTRY_URL.to_string());
        Self { registry_url: url }
    }
}

impl ExtensionCatalogService {
    pub fn curated_entries() -> Vec<CatalogExtensionEntry> {
        vec![
            CatalogExtensionEntry {
                id: "community.matrix-element-tab".to_string(),
                name: "Matrix / Element Chat Tab".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Communication".to_string(),
                description: "Adds a Matrix / Element Web workspace tab next to Mail, Calendar, Contacts, and Tasks. Edit extension.json to point to your own homeserver or Cinny/Mattermost instance.".to_string(),
                icon_name: "mail-message-new-symbolic".to_string(),
                permissions: vec!["network".to_string()],
                homepage_url: "https://app.element.io".to_string(),
                download_url: None,
                builtin_template: Some("matrix-element-tab".to_string()),
            },
            CatalogExtensionEntry {
                id: "community.web-browser-tab".to_string(),
                name: "Integrated Web Browser Tab".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Workspace".to_string(),
                description: "Demonstrates full shell extensibility by adding a lightweight WebKit browser tab with Back, Forward, Reload, and an address bar right inside Meridian.".to_string(),
                icon_name: "web-browser-symbolic".to_string(),
                permissions: vec!["network".to_string()],
                homepage_url: "https://duckduckgo.com".to_string(),
                download_url: None,
                builtin_template: Some("web-browser-tab".to_string()),
            },
            CatalogExtensionEntry {
                id: "community.inline-doc-preview".to_string(),
                name: "Inline Document Renderer".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Mail Viewer".to_string(),
                description: "Renders Markdown (.md), CSV spreadsheets (.csv), JSON files (.json), and XRechnung / ZUGFeRD XML invoices directly inside the email preview.".to_string(),
                icon_name: "x-office-document-symbolic".to_string(),
                permissions: vec!["mail.attachments".to_string()],
                homepage_url: String::new(),
                download_url: None,
                builtin_template: Some("inline-doc-preview".to_string()),
            },
            CatalogExtensionEntry {
                id: "community.regex-mail-rules".to_string(),
                name: "Regex Ticket Linkifier & Badges".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Mail Viewer".to_string(),
                description: "Highlights ticket IDs (like JIRA-123 or PROJ-42) inside emails and adds automatic list badges using editable regex rules in extension.json.".to_string(),
                icon_name: "edit-find-replace-symbolic".to_string(),
                permissions: vec![
                    "mail.headers".to_string(),
                    "mail.body".to_string(),
                    "mail.modify".to_string(),
                ],
                homepage_url: String::new(),
                download_url: None,
                builtin_template: Some("regex-mail-rules".to_string()),
            },
            CatalogExtensionEntry {
                id: "community.compact-theme".to_string(),
                name: "Compact Minimalist CSS Pack".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Appearance".to_string(),
                description: "Refines message list density and email typography using editable app.css and viewer.css stylesheets (Obsidian / GNOME Shell style).".to_string(),
                icon_name: "applications-graphics-symbolic".to_string(),
                permissions: vec!["ui.style".to_string()],
                homepage_url: String::new(),
                download_url: None,
                builtin_template: Some("compact-theme".to_string()),
            },
            CatalogExtensionEntry {
                id: "community.custom-locale-pack".to_string(),
                name: "Custom Language Pack Template".to_string(),
                version: "1.0.0".to_string(),
                author: "Martin Gawron".to_string(),
                category: "Localization".to_string(),
                description: "Installs an editable locale.json translation pack so communities or users can add new UI languages or customize labels without rebuilding Meridian.".to_string(),
                icon_name: "preferences-desktop-locale-symbolic".to_string(),
                permissions: vec!["ui.style".to_string()],
                homepage_url: String::new(),
                download_url: None,
                builtin_template: Some("custom-locale-pack".to_string()),
            },
        ]
    }

    /// Fetch the catalog off the GTK main thread, merging optional remote registry entries
    /// with Meridian's curated built-in extension catalog.
    pub fn fetch_catalog_blocking(&self) -> Vec<CatalogExtensionEntry> {
        let mut entries = Self::curated_entries();
        let mut seen: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();

        if self.registry_url.starts_with("http://") || self.registry_url.starts_with("https://") {
            if let Ok(client) = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(4))
                .user_agent("Meridian-ExtensionCatalog/1.0")
                .build()
            {
                if let Ok(resp) = client.get(&self.registry_url).send() {
                    if resp.status().is_success() {
                        if let Ok(payload) = resp.json::<RemoteCatalogPayload>() {
                            for entry in payload.extensions {
                                if !entry.id.trim().is_empty() && seen.insert(entry.id.clone()) {
                                    entries.push(entry);
                                }
                            }
                        }
                    }
                }
            }
        }
        entries
    }

    /// Install a catalog entry into the user's extensions folder as real, editable files.
    pub fn install_entry(
        &self,
        registry: &ExtensionRegistry,
        entry: &CatalogExtensionEntry,
    ) -> Result<ExtensionManifest, String> {
        let safe_id = sanitize_extension_id(&entry.id);
        let dest_dir = registry.extensions_directory().join(&safe_id);
        fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("Could not create extension directory: {e}"))?;

        if let Some(template) = entry.builtin_template.as_deref() {
            let (manifest, extra_files) = build_catalog_template(template, entry)?;
            validate_manifest(&manifest)?;
            let manifest_json =
                serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
            fs::write(dest_dir.join("extension.json"), manifest_json)
                .map_err(|e| format!("Failed writing extension.json: {e}"))?;
            for (rel_path, content) in extra_files {
                let target = dest_dir.join(rel_path);
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                fs::write(&target, content)
                    .map_err(|e| format!("Failed writing extension asset: {e}"))?;
            }
            let mut state = registry.load_state();
            state.enabled_overrides.insert(manifest.id.clone(), true);
            let _ = registry.save_state(&state);
            notify_extensions_changed();
            return Ok(manifest);
        }

        if let Some(url) = entry.download_url.as_deref() {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(12))
                .user_agent("Meridian-ExtensionCatalog/1.0")
                .build()
                .map_err(|e| e.to_string())?;
            let resp = client
                .get(url)
                .send()
                .map_err(|e| format!("Download failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("Download returned HTTP {}", resp.status()));
            }
            let raw = resp.text().map_err(|e| e.to_string())?;
            let manifest: ExtensionManifest = serde_json::from_str(&raw)
                .map_err(|e| format!("Invalid remote extension.json: {e}"))?;
            validate_manifest(&manifest)?;
            let reviewed_perms: HashSet<ExtensionPermission> =
                entry.audited_permissions().into_iter().collect();
            let unexpected: Vec<&'static str> = manifest
                .audited_permissions()
                .into_iter()
                .filter(|p| !reviewed_perms.contains(p))
                .map(ExtensionPermission::as_str)
                .collect();
            if !unexpected.is_empty() {
                return Err(format!(
                    "Downloaded extension requests unreviewed permissions: {}",
                    unexpected.join(", ")
                ));
            }
            fs::write(dest_dir.join("extension.json"), raw)
                .map_err(|e| format!("Failed writing extension.json: {e}"))?;
            let mut state = registry.load_state();
            state.enabled_overrides.insert(manifest.id.clone(), true);
            let _ = registry.save_state(&state);
            notify_extensions_changed();
            return Ok(manifest);
        }

        Err("Catalog entry has neither a built-in template nor a download URL".to_string())
    }
}

fn build_catalog_template(
    template: &str,
    entry: &CatalogExtensionEntry,
) -> Result<(ExtensionManifest, Vec<(String, String)>), String> {
    match template {
        "matrix-element-tab" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    views: vec![ExtensionViewContribution {
                        id: "matrix-chat".to_string(),
                        title: "{{setting:tab_title}}".to_string(),
                        icon_name: "mail-message-new-symbolic".to_string(),
                        url: "{{setting:server_url}}".to_string(),
                        html_file: String::new(),
                        show_navigation_bar: false,
                        allow_javascript: true,
                        placement: ExtensionViewPlacement::Tab,
                        surfaces: Vec::new(),
                        default_width: None,
                    }],
                    settings: vec![
                        ExtensionSettingSchema {
                            key: "server_url".to_string(),
                            title: "Web App URL".to_string(),
                            description: "Element, Cinny, Mattermost, or Zulip URL to load in this tab.".to_string(),
                            kind: ExtensionSettingKind::String,
                            default_value: "https://app.element.io".to_string(),
                        },
                        ExtensionSettingSchema {
                            key: "tab_title".to_string(),
                            title: "Tab Title".to_string(),
                            description: "Label shown in the top switcher bar.".to_string(),
                            kind: ExtensionSettingKind::String,
                            default_value: "Matrix".to_string(),
                        },
                    ],
                    ..ExtensionContributions::default()
                },
            },
            Vec::new(),
        )),
        "web-browser-tab" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    views: vec![ExtensionViewContribution {
                        id: "web-browser".to_string(),
                        title: "Browser".to_string(),
                        icon_name: "web-browser-symbolic".to_string(),
                        url: "{{setting:home_url}}".to_string(),
                        html_file: String::new(),
                        show_navigation_bar: true,
                        allow_javascript: true,
                        placement: ExtensionViewPlacement::Tab,
                        surfaces: Vec::new(),
                        default_width: None,
                    }],
                    settings: vec![ExtensionSettingSchema {
                        key: "home_url".to_string(),
                        title: "Home Page URL".to_string(),
                        description: "Initial page loaded when opening the Browser tab.".to_string(),
                        kind: ExtensionSettingKind::String,
                        default_value: "https://duckduckgo.com".to_string(),
                    }],
                    ..ExtensionContributions::default()
                },
            },
            Vec::new(),
        )),
        "inline-doc-preview" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    attachment_renderers: vec![
                        ExtensionAttachmentRenderer {
                            id: "inline-md".to_string(),
                            title: "Markdown Preview".to_string(),
                            extensions: vec!["md".to_string(), "markdown".to_string()],
                            mime_types: vec!["text/markdown".to_string()],
                            exec: "builtin:markdown".to_string(),
                        },
                        ExtensionAttachmentRenderer {
                            id: "inline-csv".to_string(),
                            title: "CSV Table Preview".to_string(),
                            extensions: vec!["csv".to_string()],
                            mime_types: vec!["text/csv".to_string()],
                            exec: "builtin:csv".to_string(),
                        },
                        ExtensionAttachmentRenderer {
                            id: "inline-json".to_string(),
                            title: "JSON Preview".to_string(),
                            extensions: vec!["json".to_string()],
                            mime_types: vec!["application/json".to_string()],
                            exec: "builtin:json".to_string(),
                        },
                        ExtensionAttachmentRenderer {
                            id: "inline-einvoice".to_string(),
                            title: "E-Invoice / XML Preview".to_string(),
                            extensions: vec!["xml".to_string()],
                            mime_types: vec!["application/xml".to_string(), "text/xml".to_string()],
                            exec: "builtin:e-invoice".to_string(),
                        },
                    ],
                    ..ExtensionContributions::default()
                },
            },
            Vec::new(),
        )),
        "regex-mail-rules" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    message_transformers: vec![
                        ExtensionMessageTransformer {
                            id: "ticket-chip".to_string(),
                            target: TransformerTarget::BodyHtml,
                            pattern: "{{setting:ticket_pattern}}".to_string(),
                            replacement: r#"<span style="display:inline-block;padding:1px 6px;border-radius:6px;background:rgba(53,132,228,0.16);color:#3584e4;font-weight:600;font-size:0.9em;">$1</span>"#.to_string(),
                            badge_color: String::new(),
                        },
                        ExtensionMessageTransformer {
                            id: "urgent-badge".to_string(),
                            target: TransformerTarget::ListBadge,
                            pattern: "{{setting:badge_pattern}}".to_string(),
                            replacement: "$1".to_string(),
                            badge_color: "#e01b24".to_string(),
                        },
                    ],
                    settings: vec![
                        ExtensionSettingSchema {
                            key: "ticket_pattern".to_string(),
                            title: "Ticket Regex (Body Highlight)".to_string(),
                            description: "Matches ticket IDs inside emails and highlights capture group $1.".to_string(),
                            kind: ExtensionSettingKind::String,
                            default_value: r"\b([A-Z]{2,8}-\d{1,6})\b".to_string(),
                        },
                        ExtensionSettingSchema {
                            key: "badge_pattern".to_string(),
                            title: "Mail List Badge Regex".to_string(),
                            description: "Matches subjects/senders to display capture group $1 as a badge in the mail list.".to_string(),
                            kind: ExtensionSettingKind::String,
                            default_value: r"(?i)\[(URGENT|ACTION|INVOICE)\]".to_string(),
                        },
                    ],
                    ..ExtensionContributions::default()
                },
            },
            Vec::new(),
        )),
        "compact-theme" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    stylesheets: vec![
                        ExtensionStylesheetContribution {
                            target: StylesheetTarget::App,
                            path: "app.css".to_string(),
                            css: String::new(),
                        },
                        ExtensionStylesheetContribution {
                            target: StylesheetTarget::MailViewer,
                            path: "viewer.css".to_string(),
                            css: String::new(),
                        },
                    ],
                    ..ExtensionContributions::default()
                },
            },
            vec![
                (
                    "app.css".to_string(),
                    "/* Compact Minimalist GTK4 overrides — edit freely */\n.mail-list-row { padding-top: 3px; padding-bottom: 3px; }\n".to_string(),
                ),
                (
                    "viewer.css".to_string(),
                    "/* Compact MailViewer overrides — edit freely */\nbody { line-height: 1.5; letter-spacing: 0.005em; }\nblockquote { border-left: 3px solid rgba(53,132,228,0.55); padding-left: 10px; opacity: 0.88; }\n".to_string(),
                ),
            ],
        )),
        "custom-locale-pack" => Ok((
            ExtensionManifest {
                id: entry.id.clone(),
                name: entry.name.clone(),
                version: entry.version.clone(),
                author: entry.author.clone(),
                description: entry.description.clone(),
                icon_name: entry.icon_name.clone(),
                enabled_by_default: true,
                permissions: entry.permissions.clone(),
                contributes: ExtensionContributions {
                    locales: vec![ExtensionLocaleContribution {
                        code: "*".to_string(),
                        name: "Custom UI Overrides".to_string(),
                        json_file: "locale.json".to_string(),
                    }],
                    ..ExtensionContributions::default()
                },
            },
            vec![(
                "locale.json".to_string(),
                serde_json::to_string_pretty(&serde_json::json!({
                    "_comment": "Map English Meridian UI strings to your preferred language. Set 'code' in extension.json to a language code like 'ja', 'ko', 'zh_CN', or '*' for always-active overrides.",
                    "_example_New Mail": "Compose"
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            )],
        )),
        other => Err(format!("Unknown catalog template '{other}'")),
    }
}

fn sanitize_extension_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "extension".to_string()
    } else {
        trimmed.to_string()
    }
}

fn copy_dir_flat_or_nested(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            continue;
        }
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_flat_or_nested(&entry.path(), &target)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ─── Built-in RSS / Atom Feed Store & Parser (powers `meridian.rss-feeds`) ───

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RssFeedSubscription {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub last_synced_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RssFeedArticle {
    pub id: String,
    pub feed_id: String,
    pub feed_title: String,
    pub title: String,
    pub link: String,
    pub published: String,
    pub summary_html: String,
    #[serde(default)]
    pub is_read: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RssStoreFile {
    version: u32,
    #[serde(default)]
    feeds: Vec<RssFeedSubscription>,
    #[serde(default)]
    articles: Vec<RssFeedArticle>,
}

#[derive(Debug, Clone)]
pub struct RssFeedStore {
    path: PathBuf,
}

impl Default for RssFeedStore {
    fn default() -> Self {
        Self {
            path: super::app_data_dir().join("rss-feeds.json"),
        }
    }
}

impl RssFeedStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load_file(&self) -> RssStoreFile {
        let Ok(raw) = fs::read(&self.path) else {
            return RssStoreFile {
                version: RSS_STORE_VERSION,
                feeds: Vec::new(),
                articles: Vec::new(),
            };
        };
        serde_json::from_slice(&raw).unwrap_or(RssStoreFile {
            version: RSS_STORE_VERSION,
            feeds: Vec::new(),
            articles: Vec::new(),
        })
    }

    fn save_file(&self, data: &RssStoreFile) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec_pretty(data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, encoded)?;
        fs::rename(tmp, &self.path)
    }

    pub fn subscriptions(&self) -> Vec<RssFeedSubscription> {
        self.load_file().feeds
    }

    pub fn articles(&self) -> Vec<RssFeedArticle> {
        self.load_file().articles
    }

    pub fn add_subscription(&self, title: &str, url: &str) -> Result<RssFeedSubscription, String> {
        let clean_url = url.trim();
        if !(clean_url.starts_with("https://") || clean_url.starts_with("http://")) {
            return Err("Feed URL must start with http:// or https://".to_string());
        }
        let mut data = self.load_file();
        if let Some(existing) = data
            .feeds
            .iter()
            .find(|f| f.url.eq_ignore_ascii_case(clean_url))
        {
            return Ok(existing.clone());
        }
        let sub = RssFeedSubscription {
            id: uuid::Uuid::new_v4().to_string(),
            title: if title.trim().is_empty() {
                clean_url.to_string()
            } else {
                title.trim().to_string()
            },
            url: clean_url.to_string(),
            last_synced_utc: None,
        };
        data.feeds.push(sub.clone());
        self.save_file(&data).map_err(|e| e.to_string())?;
        Ok(sub)
    }

    pub fn remove_subscription(&self, feed_id: &str) -> io::Result<()> {
        let mut data = self.load_file();
        data.feeds.retain(|f| f.id != feed_id);
        data.articles.retain(|a| a.feed_id != feed_id);
        self.save_file(&data)
    }

    pub fn mark_article_read(&self, article_id: &str, is_read: bool) -> io::Result<()> {
        let mut data = self.load_file();
        if let Some(art) = data.articles.iter_mut().find(|a| a.id == article_id) {
            art.is_read = is_read;
            self.save_file(&data)?;
        }
        Ok(())
    }

    /// Fetch and parse all subscribed RSS 2.0 / Atom 1.0 feeds off the main thread.
    pub fn refresh_all_blocking(&self) -> Result<usize, String> {
        let mut data = self.load_file();
        if data.feeds.is_empty() {
            return Ok(0);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent("Meridian-RSS/1.0")
            .build()
            .map_err(|e| e.to_string())?;

        let mut new_count = 0usize;
        let existing_ids: HashSet<String> = data.articles.iter().map(|a| a.id.clone()).collect();
        let mut seen = existing_ids;

        for feed in &mut data.feeds {
            let resp = match client.get(&feed.url).send() {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };
            let Ok(xml) = resp.text() else {
                continue;
            };
            let (detected_title, parsed) = parse_rss_or_atom_xml(&xml, &feed.id, &feed.title);
            if !detected_title.is_empty() && feed.title == feed.url {
                feed.title = detected_title;
            }
            feed.last_synced_utc = Some(Utc::now());
            for item in parsed {
                if seen.insert(item.id.clone()) {
                    data.articles.insert(0, item);
                    new_count += 1;
                }
            }
        }
        if data.articles.len() > 500 {
            data.articles.truncate(500);
        }
        self.save_file(&data).map_err(|e| e.to_string())?;
        Ok(new_count)
    }
}

/// Lightweight, dependency-free RSS 2.0 and Atom 1.0 parser.
pub fn parse_rss_or_atom_xml(
    xml: &str,
    feed_id: &str,
    fallback_feed_title: &str,
) -> (String, Vec<RssFeedArticle>) {
    let channel_header = xml
        .split("<item")
        .next()
        .and_then(|s| s.split("<entry").next())
        .unwrap_or(xml);
    let feed_title = extract_xml_tag(channel_header, "title")
        .map(|s| decode_xml_entities(&s))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback_feed_title.to_string());

    let mut articles = Vec::new();
    let is_atom = xml.contains("<entry") && !xml.contains("<item");
    let tag = if is_atom { "entry" } else { "item" };

    for block in extract_xml_blocks(xml, tag) {
        let title = extract_xml_tag(block, "title")
            .map(|s| decode_xml_entities(&s))
            .unwrap_or_else(|| "(Untitled)".to_string());
        let link = if is_atom {
            extract_atom_link(block).unwrap_or_default()
        } else {
            extract_xml_tag(block, "link")
                .map(|s| decode_xml_entities(&s))
                .unwrap_or_default()
        };
        let published = extract_xml_tag(block, "pubDate")
            .or_else(|| extract_xml_tag(block, "published"))
            .or_else(|| extract_xml_tag(block, "updated"))
            .map(|s| decode_xml_entities(&s))
            .unwrap_or_default();
        let raw_summary = extract_xml_tag(block, "content:encoded")
            .or_else(|| extract_xml_tag(block, "content"))
            .or_else(|| extract_xml_tag(block, "description"))
            .or_else(|| extract_xml_tag(block, "summary"))
            .map(|s| decode_xml_entities(&s))
            .unwrap_or_default();
        let guid = extract_xml_tag(block, "guid")
            .or_else(|| extract_xml_tag(block, "id"))
            .map(|s| decode_xml_entities(&s))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}:{}", link, title));

        articles.push(RssFeedArticle {
            id: format!("{feed_id}::{guid}"),
            feed_id: feed_id.to_string(),
            feed_title: feed_title.clone(),
            title: title.trim().to_string(),
            link: link.trim().to_string(),
            published: published.trim().to_string(),
            summary_html: super::html_sanitizer::sanitize_html(&raw_summary),
            is_read: false,
        });
    }

    (feed_title, articles)
}

fn extract_xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");
    let mut results = Vec::new();
    let mut cursor = xml;
    while let Some(start) = cursor.find(&open_prefix) {
        let after_open = &cursor[start + open_prefix.len()..];
        let valid_boundary = after_open
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace());
        if !valid_boundary {
            cursor = after_open;
            continue;
        }
        let Some(tag_end) = after_open.find('>') else {
            break;
        };
        let inner_slice = &after_open[tag_end + 1..];
        let Some(end) = inner_slice.find(&close_tag) else {
            break;
        };
        results.push(&inner_slice[..end]);
        cursor = &inner_slice[end + close_tag.len()..];
    }
    results
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let blocks = extract_xml_blocks(xml, tag);
    let raw = blocks.first()?.trim();
    if let Some(cdata) = raw
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
    {
        return Some(cdata.to_string());
    }
    Some(raw.to_string())
}

fn extract_atom_link(entry_xml: &str) -> Option<String> {
    for part in entry_xml.split("<link") {
        let Some(end) = part.find('>') else { continue };
        let attrs = &part[..end];
        if let Some(href_pos) = attrs.find("href=") {
            let rest = &attrs[href_pos + 5..];
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                let inner = &rest[1..];
                if let Some(close_q) = inner.find(quote) {
                    return Some(decode_xml_entities(&inner[..close_q]));
                }
            }
        }
    }
    None
}

fn decode_xml_entities(input: &str) -> String {
    input
        .replace("<![CDATA[", "")
        .replace("]]>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_extensions_are_disabled_by_default_and_can_be_toggled() {
        let dir = std::env::temp_dir().join(format!("meridian-ext-test-{}", uuid::Uuid::new_v4()));
        let reg = ExtensionRegistry::new(dir.join("extensions"), dir.join("state.json"));
        let all = reg.list_all();
        assert!(all.len() >= 3);
        assert!(reg.list_enabled().is_empty());

        reg.set_enabled("meridian.rss-feeds", true).unwrap();
        assert!(reg.is_enabled("meridian.rss-feeds"));
        assert_eq!(reg.enabled_sidebar_items().len(), 1);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn starter_extension_scaffolds_and_executes_script() {
        let dir =
            std::env::temp_dir().join(format!("meridian-ext-starter-{}", uuid::Uuid::new_v4()));
        let reg = ExtensionRegistry::new(dir.join("extensions"), dir.join("state.json"));
        let created = reg
            .create_starter_extension("Invoice Helper")
            .expect("starter extension created");
        assert!(created.join("extension.json").is_file());
        assert_eq!(
            reg.enabled_header_actions(ExtensionSurface::Mail).len(),
            1
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mail_merge_expands_recipient_name_and_email() {
        let body = "Hello {{first_name}} ({{email}}), welcome {{name}}!";
        let rendered = expand_mail_merge_for_recipient(body, "\"Ada Lovelace\" <ada@example.org>");
        assert_eq!(
            rendered,
            "Hello Ada (ada@example.org), welcome Ada Lovelace!"
        );
    }

    #[test]
    fn rss_and_atom_xml_parse_cleanly() {
        let rss = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel><title>GNOME News</title>
        <item><title>GTK 4 Release</title><link>https://gnome.org/gtk4</link><guid>1</guid>
        <description><![CDATA[<p>Fast &amp; modern</p>]]></description></item>
        </channel></rss>"#;
        let (title, items) = parse_rss_or_atom_xml(rss, "f1", "Fallback");
        assert_eq!(title, "GNOME News");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "GTK 4 Release");
        assert_eq!(items[0].link, "https://gnome.org/gtk4");
    }

    #[test]
    fn catalog_install_and_v2_contributions_work_end_to_end() {
        let dir = std::env::temp_dir().join(format!("meridian-ext-v2-{}", uuid::Uuid::new_v4()));
        let reg = ExtensionRegistry::new(dir.join("extensions"), dir.join("state.json"));
        let catalog = ExtensionCatalogService::default();
        let entries = ExtensionCatalogService::curated_entries();

        // 1. Install Matrix tab + Web Browser tab
        let matrix = entries
            .iter()
            .find(|e| e.id == "community.matrix-element-tab")
            .unwrap();
        catalog.install_entry(&reg, matrix).unwrap();
        let views = reg.enabled_views();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].1.title, "Matrix");
        assert_eq!(views[0].1.url, "https://app.element.io");

        // 2. Install Regex rules and verify HTML tag attributes are preserved
        let regex_ext = entries
            .iter()
            .find(|e| e.id == "community.regex-mail-rules")
            .unwrap();
        catalog.install_entry(&reg, regex_ext).unwrap();
        let transformed = reg.apply_message_html_transformers(
            r#"<p data-id="PROJ-99">See PROJ-123 for details</p>"#,
        );
        assert!(transformed.contains(r#"data-id="PROJ-99""#));
        assert!(transformed.contains(">PROJ-123</span>"));
        let badges = reg.message_list_badges("[URGENT] Server alert", "ops@example.org");
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].0, "URGENT");

        // 3. Install Inline Document Renderer and verify MD, CSV, JSON & E-Invoice XML
        let doc_ext = entries
            .iter()
            .find(|e| e.id == "community.inline-doc-preview")
            .unwrap();
        catalog.install_entry(&reg, doc_ext).unwrap();
        let (_, md_html) = reg
            .render_inline_attachment("notes.md", "text/markdown", b"# Hello\n- **Item**")
            .expect("markdown rendered");
        assert!(md_html.contains("<h2>Hello</h2>"));
        assert!(md_html.contains("<strong>Item</strong>"));

        let (_, csv_html) = reg
            .render_inline_attachment("report.csv", "text/csv", b"Name,Total\nAlice,42")
            .expect("csv rendered");
        assert!(csv_html.contains("<table"));
        assert!(csv_html.contains("Alice"));

        let (_, inv_html) = reg
            .render_inline_attachment(
                "invoice.xml",
                "application/xml",
                b"<rsm:CrossIndustryInvoice><ram:ID>INV-2026-01</ram:ID><ram:GrandTotalAmount>199.00</ram:GrandTotalAmount><ram:InvoiceCurrencyCode>EUR</ram:InvoiceCurrencyCode></rsm:CrossIndustryInvoice>",
            )
            .expect("invoice rendered");
        assert!(inv_html.contains("INV-2026-01"));
        assert!(inv_html.contains("199.00"));

        // 4. Install Compact CSS Pack and verify both App and MailViewer CSS load
        let css_ext = entries
            .iter()
            .find(|e| e.id == "community.compact-theme")
            .unwrap();
        catalog.install_entry(&reg, css_ext).unwrap();
        assert!(reg.enabled_app_css().contains(".mail-list-row"));
        assert!(reg.enabled_mail_viewer_css().contains("blockquote"));

        // 5. Install Locale Pack and verify custom overrides resolve
        let loc_ext = entries
            .iter()
            .find(|e| e.id == "community.custom-locale-pack")
            .unwrap();
        catalog.install_entry(&reg, loc_ext).unwrap();
        let loc_file = reg
            .extensions_directory()
            .join("community.custom-locale-pack")
            .join("locale.json");
        fs::write(&loc_file, r#"{"New Mail": "Schreiben"}"#).unwrap();
        let overrides = reg.active_locale_overrides("de_DE.UTF-8");
        assert_eq!(overrides.get("New Mail").map(String::as_str), Some("Schreiben"));

        // 6. Placement override (Tab -> GlobalSidebar -> PageSidebar) & permission check
        reg.set_view_placement_override(
            "community.matrix-element-tab",
            "matrix-chat",
            ExtensionViewPlacement::GlobalSidebar,
        )
        .unwrap();
        let views_after = reg.enabled_views();
        assert_eq!(views_after[0].1.placement, ExtensionViewPlacement::GlobalSidebar);
        assert!(views_after[0].0.has_permission("network"));
        assert!(!views_after[0].0.has_permission("read_mail"));

        // 7. Guide generation writes LLM-ready EXTENSION_GUIDE.md
        let guide = ensure_extension_guide_written(reg.extensions_directory()).unwrap();
        let guide_text = fs::read_to_string(&guide).unwrap();
        assert!(guide_text.contains("window.meridian.getSelectedMail"));
        assert!(guide_text.contains("\"secret\""));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn permission_auditing_catches_undeclared_capabilities_and_legacy_aliases() {
        let sneaky = ExtensionManifest {
            id: "test.sneaky".to_string(),
            name: "Sneaky Helper".to_string(),
            version: "1.0.0".to_string(),
            author: "Unknown".to_string(),
            description: String::new(),
            icon_name: default_extension_icon(),
            enabled_by_default: false,
            permissions: Vec::new(), // Claims zero permissions!
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "test.sneaky.run".to_string(),
                    title: "Run".to_string(),
                    category: String::new(),
                    shortcut: String::new(),
                    exec: "script.sh".to_string(),
                    args: Vec::new(),
                }],
                attachment_renderers: vec![ExtensionAttachmentRenderer {
                    id: "r1".to_string(),
                    title: "MD".to_string(),
                    extensions: vec!["md".to_string()],
                    mime_types: Vec::new(),
                    exec: "builtin:markdown".to_string(),
                }],
                views: vec![ExtensionViewContribution {
                    id: "v1".to_string(),
                    title: "Web".to_string(),
                    icon_name: default_extension_icon(),
                    url: "https://example.org".to_string(),
                    html_file: String::new(),
                    show_navigation_bar: false,
                    allow_javascript: true,
                    placement: ExtensionViewPlacement::Tab,
                    surfaces: Vec::new(),
                    default_width: None,
                }],
                ..ExtensionContributions::default()
            },
        };

        let audited = sneaky.audited_permissions();
        assert!(audited.contains(&ExtensionPermission::Shell));
        assert!(audited.contains(&ExtensionPermission::ReadMailAttachments));
        assert!(audited.contains(&ExtensionPermission::Network));
        assert_eq!(
            ExtensionPermission::overall_risk(&audited),
            PermissionRiskLevel::HighRisk
        );
        assert_eq!(sneaky.undeclared_capabilities(), audited);

        // Legacy coarse aliases expand into granular permissions cleanly
        let legacy = InstalledExtension {
            manifest: ExtensionManifest {
                permissions: vec!["read_mail".to_string(), "modify_mail".to_string()],
                ..sneaky
            },
            root_dir: None,
            is_bundled: false,
            enabled: true,
        };
        assert!(legacy.has_permission_kind(ExtensionPermission::ReadMailHeaders));
        assert!(legacy.has_permission_kind(ExtensionPermission::ReadMailBody));
        assert!(legacy.has_permission_kind(ExtensionPermission::ReadMailAttachments));
        assert!(legacy.has_permission_kind(ExtensionPermission::ModifyMailDisplay));
        assert!(legacy.has_permission_kind(ExtensionPermission::ComposeMail));
        assert!(legacy.has_permission_kind(ExtensionPermission::WriteTasks));
        assert!(!legacy.has_permission_kind(ExtensionPermission::Shell));
    }

    #[test]
    fn runtime_enforces_permissions_across_commands_transformers_renderers_styles_and_views() {
        let dir =
            std::env::temp_dir().join(format!("meridian-ext-enforce-{}", uuid::Uuid::new_v4()));
        let reg = ExtensionRegistry::new(dir.join("extensions"), dir.join("state.json"));
        let src = dir.join("src_unprivileged");
        fs::create_dir_all(&src).unwrap();

        let manifest = ExtensionManifest {
            id: "test.unprivileged".to_string(),
            name: "Unprivileged".to_string(),
            version: "1.0.0".to_string(),
            author: "Tester".to_string(),
            description: String::new(),
            icon_name: default_extension_icon(),
            enabled_by_default: true,
            permissions: Vec::new(),
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "test.unprivileged.cmd".to_string(),
                    title: "Run".to_string(),
                    category: String::new(),
                    shortcut: String::new(),
                    exec: "run.sh".to_string(),
                    args: Vec::new(),
                }],
                views: vec![ExtensionViewContribution {
                    id: "v1".to_string(),
                    title: "Tab".to_string(),
                    icon_name: default_extension_icon(),
                    url: "https://example.org".to_string(),
                    html_file: String::new(),
                    show_navigation_bar: false,
                    allow_javascript: true,
                    placement: ExtensionViewPlacement::Tab,
                    surfaces: Vec::new(),
                    default_width: None,
                }],
                stylesheets: vec![ExtensionStylesheetContribution {
                    target: StylesheetTarget::App,
                    path: String::new(),
                    css: "window { color: red; }".to_string(),
                }],
                message_transformers: vec![
                    ExtensionMessageTransformer {
                        id: "t-body".to_string(),
                        target: TransformerTarget::BodyHtml,
                        pattern: "SECRET".to_string(),
                        replacement: "REPLACED".to_string(),
                        badge_color: String::new(),
                    },
                    ExtensionMessageTransformer {
                        id: "t-badge".to_string(),
                        target: TransformerTarget::ListBadge,
                        pattern: "URGENT".to_string(),
                        replacement: "URGENT".to_string(),
                        badge_color: String::new(),
                    },
                ],
                attachment_renderers: vec![ExtensionAttachmentRenderer {
                    id: "r-md".to_string(),
                    title: "MD".to_string(),
                    extensions: vec!["md".to_string()],
                    mime_types: vec!["text/markdown".to_string()],
                    exec: "builtin:markdown".to_string(),
                }],
                ..ExtensionContributions::default()
            },
        };
        fs::write(
            src.join("extension.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(src.join("run.sh"), "#!/bin/sh\necho '{\"toast\":\"ran\"}'\n").unwrap();

        let inspected = ExtensionRegistry::inspect_directory_manifest(&src).unwrap();
        assert!(!inspected.undeclared_capabilities().is_empty());

        reg.install_from_directory(&src).unwrap();

        // Every single contribution must be blocked at runtime because permissions are omitted
        assert!(reg.enabled_views().is_empty());
        assert!(reg.enabled_app_css().is_empty());
        assert_eq!(
            reg.apply_message_html_transformers("<p>SECRET</p>"),
            "<p>SECRET</p>"
        );
        assert!(reg.message_list_badges("URGENT", "a@example.org").is_empty());
        assert!(reg
            .render_inline_attachment("note.md", "text/markdown", b"# Hi")
            .is_none());

        let cmd_err = reg
            .execute_command("test.unprivileged.cmd", &ExtensionExecutionContext::default())
            .unwrap_err();
        assert!(cmd_err.contains("Permission denied"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn execution_context_and_host_response_are_redacted_by_permissions() {
        let headers_only = InstalledExtension {
            manifest: ExtensionManifest {
                id: "test.headers".to_string(),
                name: "Headers Only".to_string(),
                version: "1.0.0".to_string(),
                author: "Tester".to_string(),
                description: String::new(),
                icon_name: default_extension_icon(),
                enabled_by_default: true,
                permissions: vec!["shell".to_string(), "mail.headers".to_string()],
                contributes: ExtensionContributions::default(),
            },
            root_dir: None,
            is_bundled: false,
            enabled: true,
        };

        let ctx = ExtensionExecutionContext {
            command_id: "c1".to_string(),
            active_surface: "mail".to_string(),
            message_id: Some("m1".to_string()),
            message_subject: Some("Quarterly Plan".to_string()),
            message_sender: Some("alex@example.org".to_string()),
            message_recipients: Some("team@example.org".to_string()),
            message_date: Some("2026-11-26".to_string()),
            message_body_text: Some("Confidential body text".to_string()),
            composer_to: Some("draft@example.org".to_string()),
            composer_subject: Some("Draft Subject".to_string()),
            composer_body_html: Some("<p>Draft</p>".to_string()),
        };

        let redacted = redact_execution_context(&headers_only, &ctx);
        assert_eq!(redacted.message_subject.as_deref(), Some("Quarterly Plan"));
        assert_eq!(redacted.message_sender.as_deref(), Some("alex@example.org"));
        assert!(redacted.message_body_text.is_none());
        assert!(redacted.composer_to.is_none());
        assert!(redacted.composer_body_html.is_none());

        let resp = ExtensionHostResponse {
            toast: Some("Done".to_string()),
            open_url: Some("https://evil.example".to_string()),
            insert_composer_html: Some("<p>Injected</p>".to_string()),
            replace_composer_html: Some("<p>Replaced</p>".to_string()),
            replace_composer_subject: Some("Hijacked".to_string()),
            add_label: Some("VIP".to_string()),
            create_task_title: Some("Sneaky task".to_string()),
            output_title: Some("Result".to_string()),
            output_body: Some("OK".to_string()),
        };
        let sanitized = sanitize_host_response(&headers_only, resp);
        assert_eq!(sanitized.toast.as_deref(), Some("Done"));
        assert_eq!(sanitized.output_body.as_deref(), Some("OK"));
        assert!(sanitized.open_url.is_none());
        assert!(sanitized.insert_composer_html.is_none());
        assert!(sanitized.replace_composer_html.is_none());
        assert!(sanitized.replace_composer_subject.is_none());
        assert!(sanitized.add_label.is_none());
        assert!(sanitized.create_task_title.is_none());
    }
}
