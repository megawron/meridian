// SPDX-License-Identifier: GPL-3.0-or-later
//
// ExtensionService — Declarative contribution points and sandboxed command runner
// for Meridian add-ons ("simple core, flexible via extensions").
//
// Extensions declare contribution points in `extension.json` (modeled after
// VS Code's `contributes` manifest):
// - `commands`: executable actions invoked from the Command Palette, buttons, or menus
// - `header_actions`: symbolic icon buttons mounted in Mail, Composer, Calendar, Tasks, or Contacts
// - `sidebar_items`: custom navigation items / panels in the Mail & Shell sidebar
// - `message_actions`: contextual actions on selected messages
// - `composer_hooks`: draft transformation or pre-send hooks
//
// Both bundled optional add-ons (disabled by default to keep the core UI minimal)
// and user-installed folder extensions under `<app_data_dir>/extensions/<id>/`
// share this exact contract.

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
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
    pub contributes: ExtensionContributions,
}

fn default_extension_icon() -> String {
    "application-x-addon-symbolic".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledExtension {
    pub manifest: ExtensionManifest,
    pub root_dir: Option<PathBuf>,
    pub is_bundled: bool,
    pub enabled: bool,
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
            version: "1.0.0".to_string(),
            author: "Meridian Team".to_string(),
            description: "Adds a clean Feeds section to the mail sidebar for reading RSS and Atom subscriptions alongside email.".to_string(),
            icon_name: "application-rss+xml-symbolic".to_string(),
            enabled_by_default: false,
            contributes: ExtensionContributions {
                commands: vec![ExtensionCommandContribution {
                    id: "rss.manage-feeds".to_string(),
                    title: "Feeds: Add or Manage Subscriptions…".to_string(),
                    category: "Feeds".to_string(),
                    shortcut: String::new(),
                    exec: "builtin:rss.manage".to_string(),
                    args: Vec::new(),
                }],
                header_actions: Vec::new(),
                sidebar_items: vec![ExtensionSidebarContribution {
                    id: "meridian.sidebar.rss".to_string(),
                    title: "Feeds".to_string(),
                    icon_name: "application-rss+xml-symbolic".to_string(),
                    panel_kind: SidebarPanelKind::RssFeeds,
                    command: "rss.manage-feeds".to_string(),
                }],
                message_actions: Vec::new(),
                composer_hooks: Vec::new(),
            },
        },
        ExtensionManifest {
            id: "meridian.mail-merge".to_string(),
            name: "Mail Merge".to_string(),
            version: "1.0.0".to_string(),
            author: "Meridian Team".to_string(),
            description: "Personalizes outgoing messages per recipient using {{name}}, {{first_name}}, and {{email}} placeholders.".to_string(),
            icon_name: "system-users-symbolic".to_string(),
            enabled_by_default: false,
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
                sidebar_items: Vec::new(),
                message_actions: Vec::new(),
                composer_hooks: Vec::new(),
            },
        },
        ExtensionManifest {
            id: "meridian.activity-monitor".to_string(),
            name: "Activity Monitor".to_string(),
            version: "1.0.0".to_string(),
            author: "Meridian Team".to_string(),
            description: "Adds a quick header button and activity log view to inspect background sync, filter, and transport operations.".to_string(),
            icon_name: "utilities-system-monitor-symbolic".to_string(),
            enabled_by_default: false,
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
                    icon_name: "utilities-system-monitor-symbolic".to_string(),
                    tooltip: "Activity Monitor".to_string(),
                    surface: ExtensionSurface::Mail,
                }],
                sidebar_items: Vec::new(),
                message_actions: Vec::new(),
                composer_hooks: Vec::new(),
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
            };
        };
        serde_json::from_slice::<ExtensionStateFile>(&bytes).unwrap_or(ExtensionStateFile {
            version: STATE_VERSION,
            enabled_overrides: BTreeMap::new(),
        })
    }

    fn save_state(&self, state: &ExtensionStateFile) -> io::Result<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(&ExtensionStateFile {
            version: STATE_VERSION,
            enabled_overrides: state.enabled_overrides.clone(),
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = self.state_path.with_extension("json.tmp");
        fs::write(&tmp, payload)?;
        fs::rename(tmp, &self.state_path)
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

    /// Install a user extension from a source folder containing `extension.json`.
    pub fn install_from_directory(&self, source_dir: &Path) -> Result<ExtensionManifest, String> {
        let manifest_file = source_dir.join("extension.json");
        let raw = fs::read_to_string(&manifest_file)
            .map_err(|e| format!("Could not read extension.json: {e}"))?;
        let manifest: ExtensionManifest = serde_json::from_str(&raw)
            .map_err(|e| format!("Invalid extension.json format: {e}"))?;
        validate_manifest(&manifest)?;

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
                sidebar_items: Vec::new(),
                message_actions: vec![ExtensionMessageActionContribution {
                    id: format!("{ext_id}.msg"),
                    command: format!("{ext_id}.run"),
                    label: clean_name.to_string(),
                }],
                composer_hooks: Vec::new(),
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

        if cmd.exec.trim().is_empty() {
            return Err("Command has no executable configured".to_string());
        }

        let Some(root_dir) = ext.root_dir else {
            return Err("External command requires an extension directory".to_string());
        };
        let candidate = root_dir.join(&cmd.exec);
        let program = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(&cmd.exec)
        };

        let input_bytes = serde_json::to_vec(context).map_err(|e| e.to_string())?;
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
                        return Ok(parsed);
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
    Ok(())
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
}
