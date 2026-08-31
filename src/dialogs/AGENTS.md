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
- The shortcuts window mirrors the registered application accelerators and
  must not advertise a conflicting or stale key binding.
- Destructive actions require the shared confirmation path and must report
  backend conflicts or read-only limitations.
- Settings editors must persist values through `AppSettings` and update open
  views through change notifications where the setting is live.
- The normal mail-header editor preserves Evolution's ordered enabled-state
  list and provides add, remove, enable/disable, move and reset operations.
  Reject duplicate or invalid field names before persistence.
- Composer autosave, MIME assembly, transport, and remote draft cleanup run
  outside the GTK main thread. Preserve a complete recovery draft before send,
  reject duplicate send starts, and remove the draft only after confirmed
  transport success; cancellation and failure keep the editor recoverable.
- Composer menu, toolbar and local shortcuts use one action graph. Track dirty
  revisions across recipients, body, attachments and per-message options;
  clean close is immediate, dirty close offers save/discard/cancel, and active
  save/send/export operations block conflicting actions. Save-as EML is atomic.
  Receipt, priority, charset, MIME mode and security choices round-trip through
  recovery drafts without overwriting application defaults.
- The shared alarm editor stages an ordered multi-VALARM value for events,
  tasks and memos. Add/edit/remove/reorder operations must preserve unsupported
  actions, unknown triggers and safe extension properties; unsafe imported
  actions remain visible data and are never executed by the dialog.
- Contact-list editing uses the same atomic save contract: list name/options
  and local/external/nested members are edited as one pending value, invalid
  members remain visible with a validation error, and the original record is
  unchanged when validation, cancellation, or the backend write fails.
- Calendar purge uses Evolution's destructive 0..1000-day prompt with a
  60-day default, runs outside GTK, supports cancellation, prevents duplicate
  starts, and reports removed/skipped/partial/error outcomes before triggering
  an authoritative calendar reload. Never present provider refresh as purge.
- Mail preferences expose Messages/Wide profiles and the complete normalized
  column order. Moving any column persists `custom`; applying a named profile
  restores that profile's deterministic order. The daily column popover also
  exposes both named profiles and sender/date widths.

## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `dialogs/`.
