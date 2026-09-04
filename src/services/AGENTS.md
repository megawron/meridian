# Backend and persistence services

## Purpose

Provide framework-neutral data access, parsing, synchronization, persistence,
and settings for the GTK4 frontend.

## Ownership

Own EDS/Camel/ECal/EBook bridges, source discovery, portable local stores,
mail/calendar/contact parsing, mutation commands, migration, search, and
settings persistence.

## Local Contracts

- EDS/Camel/ECal/EBook are authoritative for synchronized Linux data.
- Portable stores are explicit fallback/import paths and must preserve source
  identity and user data without pretending to be live EDS synchronization.
- App-owned mail metadata and remote-content sidecar mutations serialize the
  complete read-modify-write transaction across service instances and
  processes with a crash-released sibling-file lock. Preserve atomic data-file
  replacement, and fail without overwriting when the existing target cannot
  be read.
- Do not depend on GTK widgets or libadwaita types.
- Expose cancellation, warnings, conflicts, and backend-unavailable states so
  views can present honest status instead of hanging.
- Bind Graph HTTP futures to the caller's one-shot cancellation flag and drop
  an in-flight request promptly; checking only before and after a blocking
  request is insufficient. Source discovery persists only non-secret recovery
  timing/counters, uses bounded exponential backoff across restarts, writes
  cache state atomically, and quarantines corrupt app-owned cache files.
- Normalize mail page windows at the service boundary. Provider requests must
  use bounded page sizes and checked offsets; later pages must not download an
  ever-growing prefix. Graph continuation must preserve and validate opaque
  `@odata.nextLink` cursors instead of synthesizing positional `$skip` pages.
  Reject empty UIDs and deduplicate messages by
  `(source_uid, folder_name, message_uid)` before results reach GTK.
- Camel change streams must apply backpressure or request an explicit resync;
  a full event queue, unknown event, or malformed identifier must never stop a
  watcher silently. Preserve rename aliases for subsequent folder-scoped UID
  events until Linux can provide a stable native folder identity.
- Camel subscription APIs are exact-source and native-only. A capability probe
  distinguishes a real non-subscribable store from a failed list, recursive
  snapshots preserve parent/`NOSELECT`/subscribed state, and subscribe or
  unsubscribe returns success only after a fresh provider list confirms it.
  Never route Graph or the app-owned local store through this contract.
- Mail mutations expose a one-shot lifecycle and cooperative cancellation.
  Record undo/redo only after confirmed backend success, serialize conflicting
  work for the same message, and surface failed batch rollback explicitly.
  Worker results return completed commands to the owning UI thread; worker
  threads never mutate UI-owned history directly.
- Model the ordinary flagged/starred bit separately from Evolution follow-up.
  Follow-up retains state, text, optional `due-by`, and `completed-on` as one
  lossless value through model, sidecar, command history, and Camel parsing.
  Camel writes supported tags through the cancellable command path; Graph maps
  only the three-state value to `message.flag.flagStatus` and must reject due
  dates instead of dropping them. Local/import records use the versioned rich
  sidecar value with backward-compatible enum migration. Provider state remains
  authoritative and undo restores the exact prior metadata.
- MIME, vCard, settings, filter, and profile import paths preserve unknown safe
  data where possible. Unsupported values become redacted warnings; credentials
  and secret material must never enter app-owned stores, reports, or logs.
- Portable task filters are a typed, persistable contract: upstream-relevant
  date/status/category/attachment predicates and advanced search terms are
  normalized on load and migration, evaluated deterministically across date
  boundaries, and kept separate from native EDS query semantics.
- vCard contact lists preserve vCard 3 Evolution list properties and vCard 4
  `KIND:group`/`MEMBER` properties, including local, external, nested, and
  invalid members. Member validation is visible and atomic store writes must
  retain unrecognized safe list properties rather than silently dropping them.
- Mail printing accepts only an immutable, GTK-free selection snapshot.
  Prepare deterministic HTML/plain documents in a cancellable worker, resolve
  only loaded CID data, remove active and remote content, preserve stable
  header/attachment ordering, and expose explicit job/error states. Native
  WebKitGTK printing is a Linux presentation boundary, not service evidence.
- Composer recovery drafts persist per-message receipt, priority, charset and
  MIME/security choices without promoting them to global defaults. Explicit,
  autosave and pre-send writes use the same revision-checked atomic draft
  contract; atomic EML save must leave an existing destination untouched on
  cancellation or failure. MIME assembly validates charsets and fails closed
  on unsupported encoding or unsafe header state.
- VALARM parsing and serialization preserve ordered multiple alarms, relative
  and absolute triggers, RELATED, REPEAT/DURATION, attachments and unknown safe
  properties for VEVENT, VTODO and VJOURNAL. The scheduler may execute only
  DISPLAY delivery; AUDIO, EMAIL, PROCEDURE and unknown actions remain inert
  data. Stable deep links are service output, while application routing and a
  reminder window belong to `GW-REMINDER-RUNTIME`; native EDS watchers remain
  Linux acceptance.
- Portable calendar purge mutates only the app-owned writable calendar, uses
  the upstream 0..1000-day instant cutoff, rewrites each backing file once,
  and reports confirmed partial progress. Preserve events at the cutoff and
  fail closed for recurrence masters, detached instances, duplicate UIDs,
  durations, or unparseable bounds until native ECal can evaluate instances
  and remove by UID plus recurrence scope.
- Mail-list profiles and column order are one canonical settings contract:
  accept only known IDs, remove duplicates, append every missing column in a
  deterministic order, and mark manual reordering as `custom`. Backup and
  migration must preserve profile, exact normalized order, sender width and
  date width; legacy backups receive the installed safe defaults.
- A schema key alone is not settings parity. For each settings package record
  schema/default, preference surface, live use, backup and migration evidence
  separately, preserve deliberate secret exclusions, and report unsupported
  or missing fields instead of inferring native EDS/provider effect.
- Mail search uses one canonical query AST and matcher across quick search,
  advanced search, and saved searches. Saved-search v2 persists field, scope,
  target account/folder identities, and the versioned AST; v1 files migrate
  deterministically. Protected saved-search mutations write atomically with
  process-local RMW serialization and reject corrupt, mismatched, or duplicate
  payloads fail-closed.

## Work Guidance

- Keep native Linux adapters isolated behind small, tested interfaces.
- Keep all portable behavior deterministic and covered by unit tests.
- Keep EDS source-level writability separate from connected ECal-client
  writability: Rename/Properties use `ESource:writable`; component mutations
  use the current client's readonly state. Re-resolve exact source UID/type and
  revalidate the relevant gate inside every native worker.
- Local ECal source Properties preserve the original local backend surface:
  name, selectable color, optional custom ICS `GFile`, local writable flag and
  email address. Built-in sources reject custom files; edit through a scratch
  ESource and distinguish commit success from post-commit client availability.
- ECal task-assignment capabilities come from the connected task client, not
  source writability. Project `NO_TASK_ASSIGNMENT` and
  `NO_CONV_TO_ASSIGN_TASK` independently and fail closed for disconnected or
  non-task clients; GTK must not guess provider support from generic writable.
- Exact message-source adapters return raw bytes without UTF-8 conversion and
  honor the caller's cancellation flag. Do not retain every full MIME body in
  list records solely for source/header display.
- Header-display parsing preserves original order, repeated and unknown fields,
  unfolds continuations for display, and decodes RFC2047 values without turning
  the all-header mode into a raw source view.
- Store Evolution's normal `show-headers` contract as an ordered `a(sb)` list.
  Header names are validated, matched case-insensitively and retained by
  backup/restore and Evolution-settings migration. Graph list requests select
  and retain `internetMessageHeaders` so normal custom headers do not require a
  second provider fetch; reject injected field names and line breaks before
  constructing the reusable header block.
- Preserve safe embedded mail layout CSS while removing active/resource-loading
  declarations; external stylesheets and remote content remain policy-gated.
- Provide a conversion path for already-sanitized HTML so viewer preparation
  does not sanitize the same document twice. Long-lived viewers may cache the
  non-secret remote-content approval set and must update it only after a
  successful atomic store write.
- Worker-prepared searchable body text and crypto results must carry the same
  source/folder/message identity used by the visible record; a body replacement
  invalidates derived search text before filters are reevaluated.

## Verification

- Run service unit tests with `cargo test --workspace --locked --no-fail-fast
  --quiet`; native EDS and provider acceptance remains Linux-only.

## Child DOX Index

- No child AGENTS.md files are needed below `services/`.
