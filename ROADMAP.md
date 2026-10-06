# Meridian — Project Roadmap

A modern Mail & Groupware client for the GNOME Desktop, built with Rust, GTK4, and libadwaita, backed by Evolution Data Server (EDS).

---

## 🎯 Project Vision

Meridian serves as the modern spiritual successor to classic GNOME Evolution:
- **Consistent GNOME HIG & Libadwaita Design**: Native, responsive, accessible, and seamless on both desktop and mobile Linux.
- **Authoritative EDS Integration**: Reuses existing accounts, local caches, calendars, and address books via Evolution Data Server (`EDS`), `Camel`, `ECal`, and `EBook`.
- **Fast & Responsive**: Asynchronous background operations ensure the UI remains fluid at all times.

---

## 🚀 Current Status

- [x] **Core Shell & Navigation**: Integrated switcher for Mail, Calendar, Contacts, Tasks, Memos, and Preferences.
- [x] **Mail Client**:
  - Three-column adaptive layout (sidebar, message list, detail viewer).
  - Secure WebKitGTK HTML rendering with privacy protections (remote image blocking, sanitization).
  - Rich HTML composer with autosave and attachment support.
  - Multi-selection, batch actions, search, and threading.
- [x] **Groupware Integration (EDS)**:
  - Calendar view (Day, Work Week, Week, Month, Year, Agenda, Taskpad, Memopad).
  - Contacts view with card and table layouts.
  - Task and Memo management with status tracking.
  - Dynamic discovery of GNOME Online Accounts (GOA) and EDS sources over D-Bus.

---

## 📋 Near-Term Milestones

### 1. UI Consistency & Libadwaita Standards
- [x] Standardize all context menus across the entire application (Mail, Calendar, Tasks, Memos, Contacts) according to GNOME HIG (32px item height, 12px padding, 9px radius, 16px symbolic icons).
- [ ] Align all dialogs and preference panels with Libadwaita demo patterns (`AdwPreferencesWindow`, `AdwActionRow`).
- [ ] Continuous mobile/adaptive testing (`AdwNavigationSplitView`, `AdwBreakpoint`).

### 2. Full Groupware Editing (CRUD)
- [ ] Complete recurring calendar event editing and exception handling.
- [ ] Comprehensive contact editing (avatars, multiple emails/phones, custom fields).
- [ ] Task and memo editing dialog parity with Evolution.

### 3. Mail & Security Parity
- [ ] S/MIME and OpenPGP message signature and encryption workflows.
- [ ] Advanced server-side and client-side message filters.
- [ ] Offline synchronization and queue management.

---

## 🛠️ Development Principles

1. **GNOME HIG & Libadwaita First**: Follow standard widgets and patterns. Never introduce custom CSS that fights against Libadwaita defaults.
2. **Data Integrity**: Never corrupt or seed mock data into real user stores.
3. **100% US-English**: All code, comments, test assertions, and user-facing strings (`gettext`) are in US-English. Localizations belong in `po/`.
4. **Privacy**: Never reference personal emails or confidential files in tests or commits.
