# GTK4 views

## Purpose

Implement the visible Evolution replacement using GTK4-rs and libadwaita.

## Ownership

Own `MailPage`, `MailViewer`, calendar, contacts, tasks, memos, shared
accessibility helpers, and view-local GTK state.

## Local Contracts

- Reproduce Evolution's navigation, layout, compact row density, keyboard
  behavior, context menus, and configurable classic/modern mail arrangement.
- Use services for data access; do not embed EDS/Camel/ECal/EBook lifecycle or
  GTK types in service code.
- Keep loading and empty/error states explicit. Never leave a permanent
  spinner when a module has finished loading or failed.
- Live settings changes must update the already visible page, not only the
  next page construction.

## Work Guidance

- Prefer GTK list models/factories for repeated data and bounded main-context
  updates for large result sets.
- Keep the classic Evolution workflow available even when the default styling
  is modern or adaptive.

## Verification

- Add focused unit tests for pure view helpers and run the workspace format,
  check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `views/`.
