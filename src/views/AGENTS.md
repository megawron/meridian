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
- A folder context menu targets the exact selectable row under the secondary
  click without changing the active reader folder or starting a load. Keep the
  target only for that popover lifetime, reject whitespace/account roots, and
  expose only actions that can safely use the transient target.
- Keep message rows single-line with selection-independent actions aligned at
  the trailing edge, and cap conversation context so it cannot displace the
  message body.
- Render conversation context only from the ordered `thread_members` map
  published for the current mail-load generation. Invalidate context while a
  replacement thread model is pending, refresh it after publication, and never
  scan or sort the flat message store on selection.
- A secondary click on a mail row must select that exact row before opening
  its context menu. The menu and the main Message menu must expose the same
  daily state actions, including labels and both junk directions. Core actions
  such as move and copy must remain reachable from menus, shortcuts, and the
  classic toolbar even when the preview pane is hidden.
- Keep daily mail accelerators conflict-free and aligned with upstream:
  ordinary and unread next/previous navigation, preview, threading, print,
  source and in-message find use the same actions exposed by menus and the
  shortcuts window. Empty, unselected and boundary navigation must not select
  the wrong row.
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
  selection-sensitive follow-up editor/action exposes text, optional due date,
  clear, and complete while pending uses a marker and completed a green check.
  Carry exact metadata through undo/redo, disable unsupported Graph due dates
  visibly, fail closed for provider fields the bridge cannot write, and refresh
  only after the provider or app-owned local store confirms the mutation.
- The portable `mail-show-deleted` model materializes only app-owned local
  sidecar-deleted rows, leaves them in their logical folder, excludes them from
  unread counts, and never reruns incoming filters merely to display them.
  `Ctrl+Shift+D` restores selected local deleted rows (including an explicit
  batch) as an exact reversible command. With no deleted selection or explicit
  batch it may recover only a confirmed Delete history head. Never turn this
  into generic undo or claim provider/native deleted-row support.
- Folder ordering is a persisted presentation preference, never a provider
  mutation. Apply explicit ranks only among siblings, keep `All Messages`
  outside the real-folder level, preserve the active reader selection, and
  support reset for the current level and all levels.
- Folder Properties may use only an exact loaded local folder or exact loaded
  provider summary. Show factual counts and a human path as a read-only
  snapshot; never invent provider types, URIs, quotas, or mutable capabilities.
  Folder subscriptions target one exact loaded native Camel source. Preserve
  provider hierarchy, `NOSELECT`, loading/error/unsupported states and the
  authoritative subscribed bit; run list/mutation work off GTK, serialize it,
  and refresh only after Camel confirms the requested state. Graph and local
  mail have no subscription fallback, and no synthetic checkbox UI is allowed.
- Use services for data access; do not embed EDS/Camel/ECal/EBook lifecycle or
  GTK types in service code.
- Run every potentially blocking service mutation off the GTK main thread.
  Keep the visible model and undo/redo history unchanged until the backend
  confirms success; on error, conflict, cancellation, or worker loss, reload
  the authoritative state and leave no optimistic phantom result behind.
- Mail HTML always passes through the shared sanitizer and document-composition
  path. Remote resources are denied per message by default; CID/data content,
  base URIs, fallback text, loading, and errors must remain explicit.
- The iTIP meeting invitation banner (`ItipBanner`) renders between the message
  header and the mail body when an iCalendar/iTIP part or attachment is present,
  matching upstream Evolution's `itip-view.c`. It exposes the meeting summary,
  time/recurrence, location, organizer, attendees with participation status
  badges, a target calendar dropdown, response actions (`Annehmen`, `Vorbehalt`,
  `Ablehnen`), and a date-navigation shortcut to jump directly to the meeting in
  the Calendar page. All responses update the component or local calendar via
  `ItipService` and prepare RFC 5546 `METHOD:REPLY` envelopes without blocking GTK.
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
- Contact surfaces retain a real 0/1/n selection. Batch email, combined
  all-or-nothing vCard export, batch deletion, and card printing use the full selection.
  Delete-selection preflights all-writable sources, prompts with a named or
  counted confirmation modal, deletes sequentially stopping at the first error,
  and reduces the UI model to the confirmed deleted prefix. Delete and Backspace
  keys trigger deletion when sensitive, and Ctrl+A selects all visible contacts.
  Edit, merge, and copy remain single-contact actions.
- Contact printing provides card printing for 1 or more selected contacts and
  table printing for the visible contact list. Selected contact cards are grouped
  alphabetically with bold letter badge headings, all RFC/vCard text escaped
  strictly with GLib markup escaping, distribution list members enumerated or
  hidden according to list settings, with safe @page CSS and plain-text fallback.
  Visible contact list printing outputs a clean tabular summary. Strict preflight
  rejects empty selections/lists and duplicate contact identities. Ctrl+P activates
  card printing when contacts are selected, or visible list printing otherwise.
- Contact transfer between address books ("In Adressbuch kopieren…", "In Adressbuch verschieben…")
  mirrors upstream Evolution `e_addressbook_view_copy_to_folder` / `e_addressbook_view_move_to_folder`
  and `eab_transfer_contacts`. Both actions operate on 1 or more selected contacts.
  Copy to book is permitted from any source (including read-only); Move to book strictly
  preflights that all source address books in the selection are writable. Transfers require
  a distinct writable destination address book selected via `adw::AlertDialog` with dropdown.
  Every contact vCard is prepared with a fresh unique UID to prevent collision in the destination.
  Move enforces create-before-delete: destination records are written first, and only upon verified
  destination creation is the source record deleted. The operation aborts on the first error and
  reports precise counts of created and removed contacts.
- Find-in-message uses Evolution's `Ctrl+Shift+F` path and an in-reader bar.
  WebKit uses its FindController; plaintext fallback offsets are Unicode
  character offsets. Selection, source and render generations invalidate stale
  matches, highlights and asynchronous result signals.
- Mail print and preview actions capture one immutable selected-message
  snapshot before starting worker preparation. Keep Print, Preview, loading,
  cancellation, error and action-sensitivity states explicit; generation-gate
  worker results and never move GTK/GObject values or reparse MIME in the
  worker. The preview consumes the prepared safe document, while native
  WebKitGTK print dialogs and visual page acceptance remain Linux-only.
- Event, task and memo editors must pass the complete ordered VALARM list to
  the shared parser/serializer contract. Do not collapse multiple alarms into
  one display reminder or silently discard imported AUDIO, EMAIL, PROCEDURE,
  unknown triggers, repeat/duration fields, attachments or safe extension
  properties. Only DISPLAY alarms may reach portable notification delivery.
- Calendar move/copy creates the destination before removing the source and
  reloads authoritative data after every partial failure. Recurring occurrence
  or this-and-future deletion fails closed until the backend can persist a
  lossless exception or series split.
- Calendar purge is calendar-view-only on upstream `Ctrl+E`. The portable path
  is explicitly limited to app-owned local non-recurring events, keeps series
  and ambiguous records, surfaces confirmed/skipped/error counts, and reloads
  the authoritative view after any confirmed removal. EDS active-client purge
  remains a native provider boundary and cannot be simulated with refresh.
- Saving an unrelated task field preserves `STATUS`, `PERCENT-COMPLETE` and
  `COMPLETED` independently, including absent or unsupported imported values.
  Only explicit lifecycle-control edits normalize the coupled lifecycle fields.
- Task surfaces retain a real 0/1/n selection keyed by source plus UID. Combined
  iCalendar export must resolve every currently selected visible identity
  exactly once, validate one bare VTODO per item before opening the file chooser,
  and replace the destination atomically. Batch Complete/Incomplete preflights
  the entire writable selection, then follows upstream's per-item ECal writes:
  Complete preserves existing completion times and fills missing ones once per
  batch; Incomplete removes COMPLETED and PERCENT-COMPLETE. Partial results apply
  only UID-validated authoritative successes and report failures explicitly.
  Delete-selection uses one named/count-aware destructive confirmation, rechecks
  the exact writable selection, then removes sequentially and stops at the first
  provider error like upstream. Apply only the confirmed prefix; failed and
  unattempted rows remain. RECURRENCE-ID and the single assigned-task retract path
  fail closed until MOD_THIS/iTIP are available. Copy/Cut/Paste/Select All share
  the same visible Source+UID selection and task-list-focus shortcuts. Paste uses
  the confirmed writable default source, assigns a fresh UID to every VTODO,
  applies only the authoritative created prefix and reloads on uncertain results;
  TZID-dependent payloads fail closed until VTIMEZONE provisioning is available.
  Cut copies before entering the guarded delete path. Edit remains single-task.
  Bulk Edit remains sensitive for any non-empty selection like upstream and
  revalidates exact writable Source+UID identities on activation and dispatch.
  Every field/category change is explicitly opted in; DTSTART/DUE and
  STATUS/PERCENT-COMPLETE/COMPLETED follow the upstream coupled callbacks while
  unrelated properties remain intact. Apply sequentially, stop at the first
  provider error, rebuild visible rows from authoritative components, and reload
  on channel, reducer, or apply uncertainty. Ctrl+B is task-list-focus only;
  timezone bulk edits stay disabled until VTIMEZONE provisioning is available.
- Task Preview follows upstream's single-current-task contract: zero or multiple
  visible selections clear it, exact Source+UID revalidation precedes rendering,
  and invalid single components fail visibly. Ctrl+M toggles the persisted pane;
  Classic places it below and Vertical beside the list; each divider position is
  persisted independently. Ctrl+Shift+F is available
  only from task-list focus for one valid current task. Preview search uses Unicode
  buffer offsets and selection, component, visibility changes clear stale state.
  Preview content is inert GTK text; it must not load HTML, remote resources, or
  attachment payloads.
- Task Purge is selection-independent like upstream Ctrl+E. It confirms the exact
  current set of completed tasks across writable loaded sources, skips read-only
  sources, revalidates after confirmation, deletes sequentially, applies only the
  confirmed prefix, and reloads authoritative state when the result is uncertain.
  RECURRENCE-ID instances fail closed until native MOD_THIS is available.
- Task Forward and Open URL are exact single-selection actions. Forward validates
  the selected VTODO/UID, emits a non-mutating `METHOD:PUBLISH` envelope and
  strips VALARMs; native iTIP delivery remains a Linux handoff. Open URL reads
  the exact VTODO URL and accepts only the shared validated HTTP(S) policy before
  invoking the desktop handler; stale, mismatched and unsupported inputs fail closed.
- Task Print is an exact single-selection immutable snapshot. Its deterministic
  HTML/plain-text document escapes all VTODO fields and contains no active or
  remote content. Preparation, failure and the native GTK print-dialog handoff
  remain visibly distinct; preparing a document must never be reported as a
  completed print job.
- Task-list Print and Print Preview snapshot the actual visible row order after
  filtering and sorting, independent of selection. Every row is revalidated by
  exact Source+UID before producing escaped, remote-free HTML/plain table data;
  empty, duplicate, stale, or malformed rows fail visibly. Ctrl+P belongs only
  to task-list focus and starts list Print, while selected-task Print remains a
  separate action without that shortcut. Preparation is never a completed native
  print/preview job; GTK print dialogs and portals remain Linux acceptance.
- Task-list Refresh belongs to one exact available primary source; “All task
  lists” has no refresh target. It must not overlap task mutations. The worker
  validates the exact source UID and TASK_LIST type, checks native refresh
  support, completes the provider refresh, and only then reloads authoritative
  task state. Generation and current-source identity gate every result. A stale,
  unsupported, authentication, cancellation, or reload failure never reports
  success or replaces the visible model; local-calendar may honestly reload
  without claiming a provider refresh.
- Task source selectors and editor destinations must come from exact native
  ECal extension discovery (`TASK_LIST` for VTODO), never generic Calendar or
  GOA account discovery. Preserve each stable source UID and project connected,
  available, read-only/writable, and refresh-supported independently. A source
  whose client cannot connect remains visibly unavailable and fail-closed; it
  must not disappear, become writable by guesswork, or be replaced by a parent
  account identity.
- Task-list Copy, Rename and Delete always target one exact primary TASK_LIST
  ESource. Sensitivity comes from authoritative source-writable,
  client-writable, removable, remote-deletable and collection-owned metadata;
  source-level Rename/Properties must not inherit the ECal client's readonly
  gate used by task-data mutations. Rename and delete re-resolve the UID and
  revalidate their gates in the native worker. Delete uses remote deletion
  only when advertised. Copy selects a different exact source, overwrites
  matching component UIDs, provisions every referenced timezone before create,
  stops at the first provider error and reports only the confirmed prefix.
  Every result is generation-gated and followed by authoritative source/data
  reconciliation. The local New slice creates one exact TASK_LIST below
  `local-stub`, validates a nonblank name and `#RRGGBB` color, configures the
  local backend/selectable/local-writable extensions, commits through the EDS
  registry, and persists a requested default only from the returned UID. A
  committed source whose first client connection fails remains a successful
  create with an explicit unavailable warning. Local Properties first reloads
  that exact source, exposes name/color/default/custom ICS file and its update
  permission/email, forbids a custom file for the built-in source, commits a
  scratch ESource, and applies default-setting changes only after commit. Do
  not represent these local slices as provider-wide New or Properties parity:
  those require the provider-specific ECalSourceConfig backend surface.
- Task Assign is exact-single-selection only and fail-closed. Require the
  current Source+UID VTODO, a connected/client-writable source, and affirmative
  task-assignment plus conversion capabilities before showing or committing
  assignment fields; revalidate all of them on confirmation. Require one
  ORGANIZER and at least one ATTENDEE, preserve valid full parameterized lines,
  deduplicate short-form addresses, and replace only top-level assignment
  properties while retaining nested/folded/unknown component data. Apply the
  mutation only through the exact source worker, require authoritative EDS
  reload, and generation-gate the result. METHOD:REQUEST preparation is not
  delivery: keep actual iTIP send, delegation chains, responses and retracts
  explicitly at the Linux/provider boundary.
- Memo source selectors and editor destinations follow the same rule with the
  exact `MEMO_LIST` extension. Connected/available is not equivalent to
  writable: every remote memo mutation requires the authoritative writable
  capability, while offline/read-only/unknown sources and preserved unavailable
  filter or edit identities remain visible but fail closed. Local-calendar is
  offered only when the app-owned store confirms writability.
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
- Mail-list headers and rows use the same stable column IDs and the same
  normalized order. Messages/Wide/profile changes may rebuild realized list
  cells, but must not reload providers, filters, paging state or selection.
  Named-profile visibility follows the upstream galviews exactly. Wide and a
  Wide-derived custom order replace the separate sender/subject cells with the
  combined messages cell; Messages and Messages-derived custom orders retain
  the separate cells.
- The Calendar shell embeds a horizontal paned layout (`content_hpaned`)
  housing the calendar stack on the start side and the integrated vertical
  Taskpad/Memopad pane (`tag_vpaned`) on the end side.
- The Taskpad/Memopad pane's visibility is controlled by the GSettings key
  `calendar-show-tag-vpane` (default: `true`) and a dedicated toolbar toggle
  button (`tag_vpane_toggle_btn`). The paned position is persisted via
  `calendar-tag-vpane-position` (default: `320`) and restored live.
- Filter synchronization across the Calendar shell is unified: changes in the
  search entry or category selector synchronously filter Events in the
  calendar stack, Tasks in the Taskpad, and Memos in the Memopad, respecting
  hidden calendar sources (`calendar-hidden-sources`).
- Open-by-UID routing (`open_search_result`) searches events first, followed by
  tasks (`open_task_by_uid`), and memos (`open_memo_by_uid`), routing to the
  respective component editor via the window component contract.
- Task completion toggles in the Taskpad mutate VTODO components asynchronously
  off the GTK main thread and keep the sidebar and Taskpad views synchronized.

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
  native print dialogs/page layout, real unread-counter confirmation and EDS
  reminder watchers are Linux acceptance work documented in
  `ROADMAP_LINUX.md`; these checks must not be declared complete from macOS.

## Child DOX Index

- No child AGENTS.md files are needed below `views/`.
