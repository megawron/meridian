# GTK list-model objects

## Purpose

Represent service results as small GObject objects suitable for GTK4 list
models and selection models.

## Ownership

Own folder, mail-message, task, and EDS-source model properties and their
thread-safe-to-transfer domain values.

## Local Contracts

- Models must not own backend connections or GTK page lifecycle.
- Preserve stable source/UID identity so selection, reload, and global search
  can reopen the same Evolution record.
- Camel UIDs are folder-scoped. When a folder is renamed, update the backend
  location and synthetic row identity atomically; if the synthetic identity is
  also the thread fallback, update that fallback in the same operation.
- Keep large immutable MIME and attachment payloads reference-counted so list
  selection, rendering, saving, and crypto preflight do not duplicate buffers.
  Derived searchable body text may be cached but must be invalidated whenever
  rendered message content is replaced.
- Keep display formatting separate from authoritative service data when a
  view-specific presentation is needed.

## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `models/`.
