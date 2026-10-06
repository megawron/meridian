# Meridian — Agent & Developer Guidelines

This document defines the mandatory design, architecture, and coding rules for Meridian. All agents and developers must follow these principles strictly.

---

## 1. GNOME HIG & Libadwaita Standard (First & Foremost)

Meridian is a native GNOME application.
- **Reference Project**: The official Libadwaita source and demo project is located at `../libadwaita/demo` (and `../libadwaita/src`). Whenever creating or modifying UI elements, **always inspect the Libadwaita demo first** and replicate its widgets and layout patterns.
- **No Anti-Libadwaita CSS Hacks**: Do NOT write custom CSS overrides that fight against or shrink Libadwaita defaults. For example, never reduce standard button min-heights (e.g. forcing `26px`), never shrink font sizes (e.g. `0.90rem` or `0.76rem`), and never tamper with standard border-radii.
- **Native Widgets**: Use `AdwPreferencesWindow`, `AdwActionRow`, `AdwHeaderBar`, `AdwViewSwitcher`, `GtkPopoverMenu`, etc., instead of reinventing them with custom boxes.

---

## 2. Standardized Context Menus (Strict Specification)

All context menus across the entire application (Mail, Calendar, Tasks, Memos, Contacts, Sidebars) MUST share the exact same format and metrics:

| Property | Standard Metric |
|---|---|
| **Typography** | Default application body font (`1.0rem` / Cantarell 11pt, weight `400`). Never shrunk. |
| **Item Min-Height** | `32px` (strictly follows Libadwaita `_menus.scss` `modelbutton` standard) |
| **Item Padding** | `0 12px` (horizontal `12px`, vertically centered) |
| **Hover Border-Radius** | `9px` (`$menu_radius`) |
| **Popover Padding & Radius** | `15px` outer radius, `6px` inner margin around the stack |
| **Separators** | `margin: 6px 0` |
| **Destructive Actions** | Add `.destructive-action` class for delete/remove operations |

### 2.1 The GNOME HIG Rule on Menu Icons (Verbs vs. Nouns)

The official GNOME HIG has a strict, deliberate rule regarding icons in menus:
- **No Icons for Verbs (Actions)**: Standard menu actions representing verbs (*Reply*, *Forward*, *Save*, *Copy*, *Delete*, *Edit*) **should NOT have icons**. Icons next to every action item create visual noise/clutter and hinder scanning. GNOME core apps (Nautilus, Geary, Calendar) deliberately use clean, text-only labels for standard actions.
- **Icons for Nouns (Objects) Only**: Icons in menus are reserved for items representing concrete objects or data types (e.g. file formats, contact avatars, calendar category color dots, external accounts).
- **Inline Button Bars**: When icons are desired for basic actions (like *Cut | Copy | Paste* or *Text Formatting*), GNOME HIG recommends placing them in a compact horizontal button strip at the top of the popover (`box.inline-buttons`), rather than vertical rows with text.
- **If Icons are Present**: When an icon is used (e.g. for objects), it MUST be a `16px` symbolic icon with `8px` spacing to the label and `dim-label` styling.

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
