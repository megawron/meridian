# Meridian — Canonical Design System & UI Specification

This document is the single source of truth for all visual and interaction design decisions in Meridian. Every view, dialog, and custom widget across Mail, Calendar, Contacts, Tasks, and Memos MUST adhere to these standards so design decisions never have to be re-decided or duplicated with inconsistent metrics.

---

## 1. GNOME HIG & Libadwaita Foundation

- **Reference Implementation**: Always inspect `../libadwaita/demo` and `../libadwaita/src/stylesheet` before introducing custom CSS.
- **No Anti-Libadwaita Overrides**: Never shrink standard Libadwaita control heights (e.g., never force `26px` buttons), never shrink base font sizes (`1.0rem` / 11pt body default), and never fight native widget geometry.
- **Prefer Native Widgets**: Use `AdwPreferencesGroup`, `AdwActionRow`, `AdwEntryRow`, `AdwSwitchRow`, `AdwHeaderBar`, `AdwViewSwitcher`, `AdwBanner`, and `GtkPopoverMenu` wherever possible.

---

## 2. Cards & Boxed Lists (Exact Adwaita Demo & Hylki Parity)

All cards (`.card`) and boxed lists (`list.boxed-list`) across the entire application — including:
- **Mail Composer Metadata Card** (`.composer-header-card`)
- **Calendar / Event Composer Groups & Cards** (`.appointment-form-page list.boxed-list`, `.appointment-form-page .card`)
- **Contact / Task / Memo Editor Groups** (`adw::PreferencesGroup` / `list.boxed-list`)
- **Mail Reader Preview Card** (`.mail-preview-card`)
- **Tasks Boxed List** (`.task-boxed-list`)
- **Calendar Year View Month Cards** (`.calendar-year-month-card`)

MUST match `adwaita-1-demo` (`../libadwaita/src/stylesheet/widgets/_misc.scss` & `_lists.scss`) and `Hylki` pixel-for-pixel:

### 2.1 Do Not Override `.card` or `list.boxed-list` with Custom Borders or Flat 1px Shadows
In compiled `libadwaita-1` (`/usr/lib64/libadwaita-1.so.0`), `.card` and `list.boxed-list` use `border: none` and a 3-layer `box-shadow`:

```css
/* Exact compiled Libadwaita %card rule (Adwaita Demo & Hylki) */
background-color: var(--card-bg-color);
color: var(--card-fg-color);
border-radius: 12px;
border: none;
box-shadow: 0 0 0 1px RGB(0 0 6 / 3%),
            0 1px 3px 1px RGB(0 0 6 / 7%),
            0 2px 6px 2px RGB(0 0 6 / 3%);
```

### 2.2 Internal Row Dividers (`var(--card-shade-color)`)
In Libadwaita (`_lists.scss`), `var(--card-shade-color)` (`RGB(0 0 6 / 7%)` in Light Mode, `RGB(0 0 6 / 36%)` in Dark Mode) is used for the **internal `1px` horizontal row dividers** inside cards/boxed-lists (e.g., `list.boxed-list > row` and `.composer-header-row`):

```css
border-bottom: 1px solid var(--card-shade-color);
```
The first and last visible rows carry `12px` top/bottom corner radii (`border-top-left-radius: 12px`, `border-bottom-left-radius: 12px`), and the last visible row has `border-bottom: none`.

### 2.3 Surface Contrast (`--window-bg-color` vs `--card-bg-color`)
Parent containers hosting cards (`.mail-detail`, `window.meridian-compose-dialog`, `.appointment-form-page`, etc.) MUST use `var(--window-bg-color, @window_bg_color)` (`#fafafb` in Light Mode) and provide at least `10px–16px` margin around the card so the white card (`#ffffff` in Light Mode, `rgba(255, 255, 255, 0.08)` in Dark Mode) and its 3-layer Libadwaita shadow are never clipped or crowded.

### 2.4 Inner Card Layout Rules
- **Seamless Inner Inputs**: Inputs inside custom boxed cards (like `.composer-header-card`) are borderless and flat (`background: transparent; border: none; box-shadow: none;`), separated by `1px solid var(--card-shade-color)` row dividers (`min-height: 42px`), with focus indicated on the row (`outline: 2px solid color-mix(in srgb, var(--accent-color) 50%, transparent); outline-offset: -1px;`).

---

## 3. Borders & Paned Separators (Strict 1px Rule)

- **Single 1px Dividers**: Every structural divider between panes (sidebar, message list, preview pane, To-Do/Taskpad sidebar) MUST be exactly `1px` thick.
- **No Double Borders on `GtkPaned` Children**: `GtkPaned` already draws its own `1px` separator (`paned > separator`). Child widgets placed inside a `GtkPaned` (such as `.todo-sidebar` or `.mail-sidebar`) MUST NOT add a redundant `border-left` or `border-right` on the paned edge, as that creates a `2px` double line.

---

## 4. Banners & Inline Bars

- **Status Banners (`AdwBanner` / `.mail-status-banner`)**:
  - When a banner sits at the top of a card (e.g., *"Remote images blocked"* inside `.mail-preview-card`), it uses the card's top border radius (`12px` top-left/top-right) and a subtle tinted background (`alpha(@window_fg_color, 0.04)`).
  - **No Hard Bottom Border**: Never draw a `border-bottom` or `box-shadow` line beneath `.mail-status-banner`. Visual separation is achieved purely through the subtle background tint.

---

## 5. Mail Reader & HTML Composition (`MailViewer`)

- **White Paper Card for Email Content**: Email bodies render on a dedicated white sheet (`#ffffff` background, `#1e1e1e` text) inside `.mail-preview-card` to preserve sender HTML design and readability in both light and dark application themes.
- **Font Inheritance & Evolution Parity**:
  - Set the fallback viewer font (`Adwaita Sans`, `Cantarell`, `sans-serif`) ONLY on `html, body` (`html, body { font-family: ...; }`).
  - Do NOT force `font-family` on child selectors (`td, th, p, div, span, li, a`), so inline sender fonts on wrapper elements (e.g., `<div style="font-family: Arial, sans-serif">`) naturally inherit into table cells and paragraphs just like in Evolution.
  - Preserve the email's original quirks/standards mode (do not prepend `<!DOCTYPE html>` to doctype-less HTML fragments) so WebKit table font sizing matches Evolution.
- **Dynamic Zoom Height Synchronization**:
  - Zooming via `Ctrl` + Mousewheel, `Ctrl++`, `Ctrl+-`, or `Ctrl+0` (`zoom_in`, `zoom_out`, `zoom_reset`) MUST dynamically scale the adapted card height by `zoom_level()` and trigger an immediate height recalculation so zoomed content is never clipped.

---

## 6. Conversation / Thread Switcher & Accordion (Minimalistic & Borderless)

- **Conversation Bar (`.mail-conversation-bar`)**:
  - Minimalistic, borderless control strip above the message header: `background-color: alpha(@window_fg_color, 0.05)`, `border: none`, `border-radius: 7px`, `padding: 2px 6px`.
  - The conversation title label uses full foreground color (`@window_fg_color`, `font-weight: 600`) without `.dim-label`.
- **Thread Toggle Button (`.mail-thread-toggle-btn`)**:
  - Borderless tonal pill button (`background-color: alpha(@window_fg_color, 0.07)`, `border: none`, `padding: 1px 7px`, `font-weight: 600`).
  - When expanded (`:checked`), it highlights in accent color (`background-color: alpha(@accent_color, 0.16)`, `color: @accent_color`).
- **Thread Accordion List (`.mail-thread-accordion-list` & `.mail-thread-strip`)**:
  - Borderless, ultra-compact vertical list (`spacing: 1px`, `background: transparent`, `border: none`).
  - Each row (`.mail-thread-strip`) is `min-height: 20px`, `padding: 1px 6px`, `border: none`, with subtle hover (`alpha(@window_fg_color, 0.06)`) and active accent fill (`alpha(@accent_color, 0.14)`).
- **Message List Thread Indicators**:
  - In Cards mode, the thread count pill uses a high-contrast badge (`0.88` foreground alpha, bold text).
  - In Table mode, the thread expander chevron uses `1.9px` stroke width at `0.88` foreground alpha.

---

## 7. Standardized Context Menus

All context menus across the entire application (Mail, Calendar, Tasks, Memos, Contacts, Sidebars) MUST share the exact same metrics:

| Property | Standard Metric |
|---|---|
| **Typography** | Default application body font (`1.0rem` / 11pt, weight `400`). Never shrunk. |
| **Item Min-Height** | `30px` (or `28px` for compact table rows) |
| **Item Padding** | `0 8px` (clean `8px` left/right padding, no phantom icon gap) |
| **Hover Border-Radius** | `7px` |
| **Popover Padding & Radius** | `12px` outer radius, `4px` inner margin around items |
| **Separators** | `margin: 4px 0` |
| **Destructive Actions** | Add `.destructive-action` class for delete/remove operations |
| **Hierarchy** | Strictly single-level (flat). Never use submenus in context menus. |
| **Menu Icons** | **No icons for verbs/actions** (*Reply*, *Forward*, *Delete*, *Edit*). Icons are reserved for nouns/objects only (`16px` symbolic, `6px` spacing). |

---

## 8. Settings & Popups

Meridian follows GNOME Settings (`gnome-control-center`) and Hylki principles: **simple, non-overwhelming, yet powerful**. Progressive enhancement is strictly applied: main pages show only what 90% of users need, while power-user options reside one level deeper in sliding subpages.

| # | Rule | Description |
|---|---|---|
| **R1** | **No icons in settings rows** | Prefix icons are permitted exclusively in *object lists* (accounts with provider logos, label color dots, category colors). Navigation rows, action rows, and switch rows must not have icons. |
| **R2** | **No explanatory subtitles** | A subtitle is only permitted when it displays *dynamic data* (e.g., account email address). Explanatory prose is never allowed; row titles must be self-explanatory. |
| **R3** | **Control type follows content** | 2 states → `AdwSwitchRow`. 2–3 short, equivalent options (layout/style) → Segmented toggle (linked `ToggleButton`s) in row suffix. ≥ 4 or long options → `AdwComboRow`. Numeric value → `AdwSpinRow`. Text input → `AdwEntryRow`. |
| **R4** | **No switch-plus-dependent pairs** | A switch that merely enables or unlocks a secondary setting must be merged into it (e.g., a combo row with a "Never" / "Off" option) or implemented as an `AdwExpanderRow` with `show-enable-switch`. |
| **R5** | **Slide subpages for power-user options** | When a category has ≥ 4 power-user settings, they move to a sliding subpage. Navigation rows reside in an **untitled group at the page bottom**, containing only title + `go-next-symbolic` and an optional dim status value (e.g. "3 active"). |
| **R6** | **Dedicated dialog popups (`adw::Dialog`)** | Popups are reserved strictly for transactional editors with *Cancel/Save* semantics (account, signature, out-of-office note, filter rule, label). All popups follow the same template: `ToolbarView` + `HeaderBar` (Cancel on start, Title, Save as `suggested-action` on end), contents in `AdwPreferencesPage`, fixed sizing (**small 460×560** or **large 640×560**), and `follows-content-size = false`. Rows opening a popup end with an ellipsis ("…") and have no chevron. |
| **R7** | **Unified rich-text component** | The signature editor and out-of-office dialog (and future text templates) share the same `RichTextField` component (formatting toolbar + card with `HtmlEditor`). Bare `gtk::TextView` rectangles are prohibited. |
| **R8** | **Pages fit without scrolling** | Main settings pages must fit without vertical scrolling inside the default window dimensions (860×620): at most 3 groups, and at most ~8 rows per group. Overflow moves into subpages. |
| **R9** | **Stable dimensions** | Page stack uses `hhomogeneous = true`, all `ScrolledWindow`s enable overlay scrolling (`overlay-scrolling = true`), and content resides in standard `AdwClamp` containers — ensuring zero horizontal shifting when switching pages or when scrollbars appear. |
| **R10** | **No miniature Cairo drawings/wireframes** | Style and layout selections must use textual segmented toggle rows rather than schematic wireframes or Cairo-drawn preview cards. |
| **R11** | **Single source of truth (no duplicate settings)** | Every setting lives in exactly one place across the UI (e.g., mark-read actions, desktop notifications, per-account signatures are never duplicated). |
| **R12** | **Concise noun group titles & clean sidebar** | Group titles are short singular or plural nouns ("Notifications", "Layout"), never long "X & Y and Z" chains. Sidebar omits text section headers; blocks are separated cleanly via `ListBox::set_header_func` with `gtk::Separator` (no empty or selectable placeholder rows). |

