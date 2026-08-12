# GTK4 views

## Purpose

Implement the visible Evolution replacement using GTK4-rs and libadwaita.

## Ownership

Own `MailPage`, `MailViewer`, calendar, contacts, tasks, memos, shared
accessibility helpers, and view-local GTK state.

## Local Contracts

- Reproduce Evolution's navigation, layout, compact row density, keyboard
  behavior, context menus, and configurable classic/modern mail arrangement.
- Keep mail-account roots and their folder trees in the compact sidebar; backend
  implementation names must not appear in visible navigation or status labels.
- Keep message rows single-line with selection-independent actions aligned at
  the trailing edge, and cap conversation context so it cannot displace the
  message body.
- Keep the selected message body visually primary: the detail header and
  conversation context stay compact, while safe inline mail CSS and intended
  message backgrounds remain intact. Do not flatten provider formatting merely
  to match the application theme.
- Use services for data access; do not embed EDS/Camel/ECal/EBook lifecycle or
  GTK types in service code.
- Run every potentially blocking service mutation off the GTK main thread.
  Keep the visible model and undo/redo history unchanged until the backend
  confirms success; on error, conflict, cancellation, or worker loss, reload
  the authoritative state and leave no optimistic phantom result behind.
- Mail HTML always passes through the shared sanitizer and document-composition
  path. Remote resources are denied per message by default; CID/data content,
  base URIs, fallback text, loading, and errors must remain explicit.
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
