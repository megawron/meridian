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
- Keep display formatting separate from authoritative service data when a
  view-specific presentation is needed.

## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `models/`.
