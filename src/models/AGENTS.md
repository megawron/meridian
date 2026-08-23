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
  Preserve original RFC822 header bytes separately when available so the
  all-header view does not require retaining each complete MIME payload.
  Derived searchable body text may be cached only after worker preparation;
  the cache key must include the complete source/folder/message identity and
  must be invalidated whenever rendered or decrypted message content is
  replaced. The owning GTK filter must be notified after invalidation.
- Crypto and other asynchronous message results must carry a generation plus
  the same complete identity as the model row; equal display IDs in different
  folders or backends must never alias.
- Keep display formatting separate from authoritative service data when a
  view-specific presentation is needed.
- Keep complete follow-up metadata alongside its compatibility state field and
  update both atomically; list rendering, the editor, reload, and undo must not
  observe different follow-up states for the same message.

## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `models/`.
