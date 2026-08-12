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
