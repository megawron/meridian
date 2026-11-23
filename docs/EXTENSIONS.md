# Meridian Extension Development Guide (LLM & Developer Reference)

Create a folder inside Meridian's extensions directory containing an `extension.json` file:
- Native Linux: `~/.local/share/meridian/extensions/<your-extension-id>/extension.json`
- Flatpak: `~/.var/app/com.martingawron.Meridian/data/meridian/extensions/<your-extension-id>/extension.json`

Meridian automatically writes a local copy of this guide to `<extensions_dir>/EXTENSION_GUIDE.md` and hot-reloads extensions when toggled, installed, or configured in **Menu → Extensions**.

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
  "permissions": ["read_mail", "modify_mail"],
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
- `permissions`: optional informational badges: `"network"`, `"webview"`, `"ui_css"`, `"read_mail"`, `"modify_mail"`, `"shell"`.
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
- `await window.meridian.getSelectedMail()` *(requires `"read_mail"`)* → `{ id, subject, sender, recipients, body_text }` or `null`
- `await window.meridian.getSetting(key)` → reads any declared `string`, `bool`, or keyring-backed `secret` setting (e.g. an LLM API key)
- `await window.meridian.openReplyDraft(html)` *(requires `"modify_mail"`)* → opens Meridian's Composer prefilled as a reply
- `await window.meridian.createTask(title)` *(requires `"modify_mail"`)* → creates a task in the default task list
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
