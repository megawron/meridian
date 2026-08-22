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
  daily state actions, including labels and both junk directions. Core actions
  such as move and copy must remain reachable from menus, shortcuts, and the
  classic toolbar even when the preview pane is hidden.
- Keep the selected message body visually primary: the detail header and
  conversation context stay compact, while safe inline mail CSS and intended
  message backgrounds remain intact. Do not flatten provider formatting merely
  to match the application theme.
- Keep Evolution's message-source workflow on `Ctrl+U` and available from the
  View and row-context menus even when preview is hidden. Fetch original RFC822
  bytes outside GTK; visibly label any reconstruction. Present one reusable
  nonmodal source window per message immediately with explicit loading, source
  and error states. Closing it cancels the request, and request identity must
  reject results from a closed or replaced window. The persistent all-header
  mode extends the normal reader with bounded formatted name/value rows in
  original order; raw monospace content belongs only in the source window. It
  must not build message bodies or encode attachments on selection.
- The normal reader header uses the persistent Evolution-compatible ordered
  `show-headers` list. Enabled fields render in configured order, repeated
  values retain source order, disabled/empty fields disappear, and settings
  changes update the selected message live. The all-header mode replaces this
  compact grid instead of duplicating it.
- Prepare HTML mail bodies and their composed document off the GTK main loop.
  Every result must carry a render generation/request identity; stale results
  are discarded before they can replace the WebKit document or fallback text,
  and identical requests are coalesced. Use one persistent latest-wins worker;
  CID/Base64 work and large attachment copies must never run on selection in
  the GTK context. Body-search text is prepared in a worker path, carried
  with the complete message identity, and consumed by filters without a
  selection-time HTML-to-text conversion. Replacing rendered/decrypted content
  invalidates the body cache and emits a filter change before the new result is
  searchable.
- Perform only a bounded, cheap MIME-envelope preflight on the GTK path for
  cryptography. Run OpenPGP/S-MIME inspection only for messages that advertise
  a corresponding envelope. Hide routine unsigned/unencrypted status by
  default; a persistent opt-in may reveal it, while valid, warning, and error
  states remain visible. Crypto worker results are accepted only when both the
  request generation and the complete message identity still match.
- Keep the mail table Evolution-compact: attachment and status indicators
  lead the row, header and row cells share fixed column widths, and clicking a
  sortable header exposes the active direction there instead of requiring a
  second sort control. The first visible row is zebra-shaded. Singleton
  messages have no permanent expander; real conversations use one aligned
  count cell rather than shifting every following column. Account roots have
  no decorative icon, indentation is shallow but visible, and unread badges
  show only non-zero unread counts. Readable font defaults must not be traded
  for higher density.
- Keep the classic mail toolbar above and across the complete mail work area,
  including the folder tree. Detail-only reply actions belong above the
  subject; do not duplicate the complete classic toolbar inside the reader.
  Routine source/load status belongs at the bottom and follows the live,
  persistent status-bar preference.
- Treat the ordinary star and Evolution follow-up as separate states. The
  follow-up indicator cycles `None -> FollowUp -> Completed -> None`; pending
  uses a marker and completed uses a green check. Refresh the row only after
  the provider or app-owned local store confirms the mutation.
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
- Task filters use the persisted typed filter/search contract: date windows,
  status, category, attachments, due/start state, and advanced predicates are
  combinable and update the open view after settings or model changes. Unknown
  imported values fall back visibly instead of being treated as upstream
  parity.
- Contact-list editors expose local and external members, preserve list
  metadata and vCard 3/4 representations, mark invalid members, and refresh
  the visible member rows from the pending atomic write model.
- Find-in-message uses Evolution's `Ctrl+Shift+F` path and an in-reader bar.
  WebKit uses its FindController; plaintext fallback offsets are Unicode
  character offsets. Selection, source and render generations invalidate stale
  matches, highlights and asynchronous result signals.
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
