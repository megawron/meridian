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
- A secondary click on a mail row must select that exact row before opening
  its context menu. The menu and the main Message menu must expose the same
  daily state actions, including labels and both junk directions.
- Keep the selected message body visually primary: the detail header and
  conversation context stay compact, while safe inline mail CSS and intended
  message backgrounds remain intact. Do not flatten provider formatting merely
  to match the application theme.
- Prepare HTML mail bodies and their composed document off the GTK main loop.
  Every result must carry a render generation/request identity; stale results
  are discarded before they can replace the WebKit document or fallback text,
  and identical requests are coalesced. Use one persistent latest-wins worker;
  CID/Base64 work and large attachment copies must never run on selection in
  the GTK context.
- Perform only a bounded, cheap MIME-envelope preflight on the GTK path for
  cryptography. Run OpenPGP/S-MIME inspection only for messages that advertise
  a corresponding envelope. Hide routine unsigned/unencrypted status by
  default; a persistent opt-in may reveal it, while valid, warning, and error
  states remain visible.
- Keep the mail table Evolution-compact: attachment and status indicators
  lead the row, singleton messages have no permanent expander, account roots
  have no decorative icon, indentation is shallow, and unread badges show
  only non-zero unread counts. Readable font defaults must not be traded for
  higher density.
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
- Treat the currently requested account/folder as authoritative immediately,
  even before its worker returns. Every load, search, paging result and
  incremental model build must be generation-gated; stale results cannot
  change models or leave loading controls disabled.
- Preserve mail selection by stable ID and previous visible index. If the ID
  was removed, select the next item at that index or the previous item at the
  end; an empty model must clear header, reader, attachments, crypto status and
  conversation context.
- Apply Camel folder rename/delete events to loaded rows, request state and the
  dynamic folder tree incrementally. Rebuild large thread/list models in
  bounded main-context chunks and restore selection after refresh and paging.
  Extract only immutable primitive sort keys from mail GObjects in bounded
  chunks; sort large root lists and oversized conversations in a worker, then
  apply the returned index plan generation-safely on GTK. Never move GTK or
  GObject instances across that worker boundary.
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
- For the current mail core block, the portable gate is:
  `git diff --check`, `cargo fmt --all -- --check`,
  `cargo check --workspace --locked`, and
  `cargo test --workspace --locked --no-fail-fast --quiet`.
  Rapid HTML-message switching, WebKitGTK rendering, icon fallback, density,
  and real unread-counter confirmation are Linux acceptance work documented in
  `ROADMAP_LINUX.md`; these checks must not be declared complete from macOS.

## Child DOX Index

- No child AGENTS.md files are needed below `views/`.
