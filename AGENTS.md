# Meridian — Agent & Developer Guidelines

This document defines the mandatory design, architecture, and coding rules for Meridian. All agents and developers must follow these principles strictly.
For the complete visual specification (card shadows, borders, banners, typography, thread controls, and context menus), see **[DESIGN.md](DESIGN.md)**.

## Quick Summary for AI Agents
1. **GNOME HIG & Unified Design System ([DESIGN.md](DESIGN.md))**: Always inspect `../libadwaita/demo` for reference patterns. All cards (`.card`, `list.boxed-list`, `.composer-header-card`, Calendar composer groups, `.mail-preview-card`, `.task-boxed-list`) use Libadwaita's exact `%card` styling (`border: none`, `box-shadow: 0 0 0 1px RGB(0 0 6 / 3%), 0 1px 3px 1px RGB(0 0 6 / 7%), 0 2px 6px 2px RGB(0 0 6 / 3%)`) on `var(--window-bg-color)` (`#fafafb`) with `1px solid var(--card-shade-color)` internal row dividers. Never add double borders to `GtkPaned` children.
2. **Context Menus**: Strict standard across all views (min-height: 30px, padding: 0 8px, border-radius: 7px, standard typography, no icons for verb actions).
3. **100% US-English**: All code, comments, and `gettext` strings in US-English. Localizations in `po/`.
4. **Privacy**: Never reference real `.eml` files or local user paths. Use synthetic fixtures.
5. **Simplicity**: Keep documentation simple, actionable, and free of unnecessary overhead.

---

## 1. GNOME HIG, Libadwaita & Unified Design System (First & Foremost)

Meridian is a native GNOME application. All visual design decisions are codified in **[DESIGN.md](DESIGN.md)**:
- **Reference Project**: The official Libadwaita source and demo project is located at `../libadwaita/demo` (and `../libadwaita/src`). Whenever creating or modifying UI elements, **always inspect the Libadwaita demo first** and replicate its widgets and layout patterns.
- **No Anti-Libadwaita CSS Hacks**: Do NOT write custom CSS overrides that fight against or shrink Libadwaita defaults. For example, never reduce standard button min-heights (e.g. forcing `26px`), never shrink font sizes (e.g. `0.90rem` or `0.76rem`), and never tamper with standard border-radii.
- **Native Widgets**: Use `AdwPreferencesWindow`, `AdwPreferencesGroup`, `AdwActionRow`, `AdwHeaderBar`, `AdwViewSwitcher`, `GtkPopoverMenu`, etc., instead of reinventing them with custom boxes.
- **Unified Libadwaita Card Style (Adwaita Demo & Hylki Parity)**: Never override `.card` or `list.boxed-list` with custom borders or flat 1px shadows. All cards (`.card`, `list.boxed-list`, `.composer-header-card`, `.appointment-form-page list.boxed-list`, `.appointment-form-page .card`, `.mail-preview-card`, `.task-boxed-list`, `.calendar-year-month-card`) use Libadwaita's exact `%card` specification (`border: none`, `border-radius: 12px`, `background-color: var(--card-bg-color)`, `box-shadow: 0 0 0 1px RGB(0 0 6 / 3%), 0 1px 3px 1px RGB(0 0 6 / 7%), 0 2px 6px 2px RGB(0 0 6 / 3%)`) on a `var(--window-bg-color)` (`#fafafb`) surface, with `1px solid var(--card-shade-color)` internal row dividers.
- **Strict 1px Dividers**: Paned separators (`GtkPaned`) draw their own `1px` border. Never add redundant `border-left`/`border-right` on paned children (e.g. `.todo-sidebar`) or bottom borders on `.mail-status-banner`.

---

## 2. Standardized Context Menus (Strict Specification)

All context menus across the entire application (Mail, Calendar, Tasks, Memos, Contacts, Sidebars) MUST share the exact same format and metrics:

| Property | Standard Metric |
|---|---|
| **Typography** | Default application body font (`1.0rem` / Cantarell 11pt, weight `400`). Never shrunk. |
| **Item Min-Height** | `30px` (or `28px` for compact table rows) |
| **Item Padding** | `0 8px` (clean `8px` left/right padding, no phantom icon gap) |
| **Hover Border-Radius** | `7px` |
| **Popover Padding & Radius** | `12px` outer radius, `4px` inner margin around items |
| **Separators** | `margin: 4px 0` |
| **Destructive Actions** | Add `.destructive-action` class for delete/remove operations |
| **Hierarchy** | Strictly single-level (flat). Never use submenus or multi-page navigation in context menus. |
| **Scrolling** | Prohibited. Context menus must never scroll. Maximum 5–10 focused items. |

### 2.1 The GNOME HIG Rule on Menu Icons (Verbs vs. Nouns) & Left-Aligned Text

The official GNOME HIG has a strict, deliberate rule regarding icons in menus:
- **No Icons for Verbs (Actions)**: Standard menu actions representing verbs (*Reply*, *Forward*, *Save*, *Copy*, *Delete*, *Edit*) **should NOT have icons**. Icons next to every action item create visual noise/clutter and hinder scanning. GNOME core apps (Nautilus, Geary, Calendar) deliberately use clean, text-only labels for standard actions.
- **No Phantom Icon Spacing**: When an action menu is text-only, text MUST be aligned cleanly to the left (`padding: 0 8px`, `margin: 0`). Never reserve empty 16px/24px indentations where an icon would have been.
- **Icons for Nouns (Objects) Only**: Icons in menus are reserved for items representing concrete objects or data types (e.g. file formats, contact avatars, calendar category color dots, external accounts).
- **Inline Button Bars**: When icons are desired for basic actions (like *Cut | Copy | Paste* or *Text Formatting*), GNOME HIG recommends placing them in a compact horizontal button strip at the top of the popover (`box.inline-buttons`), rather than vertical rows with text.
- **If Icons are Present**: When an icon is used (e.g. for objects), it MUST be a `16px` symbolic icon with `6px` spacing to the label and `dim-label` styling.

---

## 3. Language & Internationalization

- **100% US-English**: All Rust code, function names, comments, docstrings, log messages, test assertions, and user-facing UI strings (`gettext("...")`, `.label(...)`, `.title(...)`) MUST be in US-English.
- **No Hardcoded German**: Hardcoded German in code is strictly prohibited. German and other localizations belong exclusively in gettext catalogs under `po/` (e.g. `po/de.po`).

---

## 4. Privacy & Test Hygiene

- **No Confidential Data**: Never read, copy, reference, or commit personal `.eml` / `.msg` files, private email addresses, or absolute local paths (e.g. `/home/...`).
- **Self-Contained Fixtures**: All unit and integration tests must use synthetic, inline RFC822 strings or mock data.
- **Git Hygiene**: `*.eml` and `*.msg` must remain in `.gitignore`.

---

## 5. Non-Blocking Architecture & Parity

- **EDS as Source of Truth**: Evolution Data Server (`EDS`), `Camel`, `ECal`, and `EBook` are the authoritative data backends.
- **Keep Main Thread Fluid**: Never perform synchronous blocking network, Camel, or disk I/O on the GTK main loop. Use background threads and asynchronous message passing.
- **Verified Operations**: Never display a success state before the backend operation has actually succeeded.
