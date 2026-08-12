# GTK4/libadwaita dialogs

## Purpose

Own transient editors, preferences, account setup, search, and confirmation
flows presented by the application shell.

## Ownership

Dialogs own their GTK4/libadwaita widget trees, validation, response handling,
and updates to the service/settings APIs.

## Local Contracts

- Use libadwaita dialog and preferences patterns; do not introduce Relm4 or a
  second frontend framework.
- Destructive actions require the shared confirmation path and must report
  backend conflicts or read-only limitations.
- Settings editors must persist values through `AppSettings` and update open
  views through change notifications where the setting is live.

## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `dialogs/`.
