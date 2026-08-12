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
- Mail mutations expose a one-shot lifecycle and cooperative cancellation.
  Record undo/redo only after confirmed backend success, serialize conflicting
  work for the same message, and surface failed batch rollback explicitly.
  Worker results return completed commands to the owning UI thread; worker
  threads never mutate UI-owned history directly.
- MIME, vCard, settings, filter, and profile import paths preserve unknown safe
  data where possible. Unsupported values become redacted warnings; credentials
  and secret material must never enter app-owned stores, reports, or logs.

## Work Guidance

- Keep native Linux adapters isolated behind small, tested interfaces.
- Keep all portable behavior deterministic and covered by unit tests.
- Preserve safe embedded mail layout CSS while removing active/resource-loading
  declarations; external stylesheets and remote content remain policy-gated.

## Verification

- Run service unit tests with `cargo test --workspace --locked --no-fail-fast
  --quiet`; native EDS and provider acceptance remains Linux-only.

## Child DOX Index

- No child AGENTS.md files are needed below `services/`.
