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
- Mail mutations expose a one-shot lifecycle and cooperative cancellation.
  Record undo/redo only after confirmed backend success, serialize conflicting
  work for the same message, and surface failed batch rollback explicitly.
  Worker results return completed commands to the owning UI thread; worker
  threads never mutate UI-owned history directly.
- Model the ordinary flagged/starred bit separately from Evolution follow-up.
  Follow-up has exactly `None`, `FollowUp`, and `Completed`: Camel maps it to
  the `follow-up`, `completed-on`, and optional `due-by` user tags; Graph maps
  it to `message.flag.flagStatus` values `notFlagged`, `flagged`, and
  `complete`. Local/import records persist the same enum in the app sidecar.
  Provider state remains authoritative and all transitions use the normal
  cancellable, backend-confirmed command path.
- MIME, vCard, settings, filter, and profile import paths preserve unknown safe
  data where possible. Unsupported values become redacted warnings; credentials
  and secret material must never enter app-owned stores, reports, or logs.

## Work Guidance

- Keep native Linux adapters isolated behind small, tested interfaces.
- Keep all portable behavior deterministic and covered by unit tests.
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

## Verification

- Run service unit tests with `cargo test --workspace --locked --no-fail-fast
  --quiet`; native EDS and provider acceptance remains Linux-only.

## Child DOX Index

- No child AGENTS.md files are needed below `services/`.
