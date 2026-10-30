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
- Account setup fills an empty email from an email-shaped username, derives a
  missing display name from the email local part, and suggests an SMTP host
  from the incoming host for IMAP/SMTP only. Explicit user values win; inferred
  fields remain visible in the form and are not proof of provider connectivity.
- Mail preferences expose Messages/Wide profiles and the complete normalized
  column order. Moving any column persists `custom`; applying a named profile
  restores that profile's deterministic order. The daily column popover also
  exposes both named profiles and sender/date widths.
- Advanced search and search folder dialogs construct only canonical query
  syntax, validate generated rules against the canonical parser before
  forwarding them to callbacks, preserve exact folder/account scopes without
  broadening, and fail closed on empty or invalid input.
- Name selector dialog (`ENameSelectorDialog`) presents contacts from configured
  address books and directory services with search-as-you-type, transfer buttons
  to To/Cc/Bcc destination models, destination removal, and synchronized updates
  back to composer recipient entry fields.
- Recipient autocompletion attaches a non-blocking GTK4 popover to recipient
  entries with Evolution's upstream 333ms debounce, generation-tracked queries
  across local vCards and GAL directory sources, keyboard-first navigation
  (Up/Down/Enter/Tab/Escape), and comma-delimited recipient token replacement.
- Filter editor dialog (`FilterEditorDialog`) supports all 23 typed filter actions
  with dynamic target inputs (folders, labels, colors, scores, sound files, programs,
  addresses). Mail preferences (`PreferencesDialog`) provide live toggles for
  `filters-log-actions` and `filters-log-file`, as well as dry-run test simulation
  triggers per rule and for all active rules without mailbox mutation.
- Group management dialog (`ManageGroupsDialog`) presents source groups with
  reordering (Move Up / Move Down), visibility toggles (show/hide), and custom
  group creation and deletion. Persists changes to domain-specific GSettings keys
  (`*-hidden-groups`, `*-groups-order`) across Calendar, Tasks, Memos, and Contacts.
- Source properties dialog (`SourcePropertiesDialog`) enables editing display names,
  color selection using native GTK4 `ColorDialogButton` with live preview dot and hex
  subtitle synchronization, and default-source toggling with confirmation callbacks.
- **Navigation & Container Architecture (GNOME HIG Decision Matrix)**:
  Follow this strict hierarchy to avoid double header bars, conflicting navigation buttons (`<`, `Cancel`, `Save`, `×`), or stretched forms:
  1. **Standalone Top-Level Window (`adw::Window` / `present_standalone` with `modal: false`)**:
     Use when the user needs to keep the main Meridian window interactive in parallel (e.g., `PreferencesDialog`, `ComposeDialog`, `EventDialog`). Settings inside `PreferencesDialog` are *instant-apply* (changes persist immediately to `GSettings` without a Save button) and therefore use a standard window close button (`×`).
  2. **Popup / Sheet Dialog (`adw::Dialog::present(Some(parent))`)**:
     Use whenever opening a **transactional unit** that requires explicit confirmation (`Cancel` on the left, `Save` / `Add` on the right) or validation before persisting, or when a self-contained narrow form (440–480px) is launched from a wide split view (980px). Never embed a transactional `Cancel`/`Save` dialog inline inside an instant-apply `PreferencesDialog` stack, as that creates stacked header bars and conflicting `<` / `Cancel` / `Save` / `×` controls. Examples: `AccountSetupDialog`, `FilterEditorDialog`, `SourcePropertiesDialog`, `adw::AlertDialog`.
  3. **Horizontal Sliding Subpage (`adw::NavigationView` + `adw::NavigationPage`)**:
     Use for hierarchical drill-down **within the same transactional or instant-apply context**, triggered by an `adw::ActionRow` with a `go-next-symbolic` (`>`) suffix when a sub-area contains **4 or more controls** or its own `AdwPreferencesGroup`s. The subpage slides horizontally over the current view and displays a single `<` back button in the existing header bar (never a second header bar or a second `Cancel`/`Save` pair). Examples: `Server Settings >` and `Synchronization >` inside `AccountSetupDialog`.
  4. **Inline Accordion (`adw::ExpanderRow`)**:
     Use only for **1 to 3 optional or rarely changed fields** that belong directly to the parent row without changing the navigation level (e.g., `Aliases & Reply-To` under Identity). Never nest `AdwExpanderRow`s or place whole multi-group forms inside an expander.
  5. **Contextual Popover (`gtk::Popover` / `adw::ComboRow`)**:
     Use for quick, single-value selections or lightweight contextual controls anchored to a specific button or row (e.g., protocol/auth dropdowns, color pickers, column profile popovers, recipient autocompletion).
- Account setup dialog (`AccountSetupDialog`) and mail preferences (`PreferencesDialog`):
  `PreferencesDialog` runs as a standalone non-modal window (`present_standalone`, 980×720) with instant-apply settings. Clicking an account or adding a new account opens `AccountSetupDialog` as a compact, transactional popup sheet (`adw::Dialog::present(Some(self))`, 440×520 for GOA accounts, 480×580 for local/shared accounts) with a single header bar (`Cancel` / `Save`). Inside `AccountSetupDialog`, the main page shows core identity and status controls (with an `AdwExpanderRow` for `Aliases & Reply-To`), while complex configurations slide horizontally via `adw::NavigationView` into dedicated `adw::NavigationPage` subpages (`Server Settings` and `Synchronization`, hidden for GOA accounts). Validation failures navigate directly to the relevant subpage (`nav_view.push_by_tag("server")`).
- Mail preferences (`PreferencesDialog`) provide dedicated sub-pages for "Farben & Labels"
  (ordered label list with color dots, Add/Edit dialogs with GTK4 `ColorDialogButton`,
  reordering, deletion with confirmation, and reset to defaults) and "Junk-Mail-Optionen"
  (incoming spam scan toggle, address book whitelist options, auto-expunge on exit with
  retention days, spam plugin selector, and custom spam headers).
- Composer dialog (`ComposeDialog`) and preferences expose full parity for composer settings:
  1. Header visibility toggles for From, Reply-To, Cc, and Bcc tied to GSettings and toggleable
     via `win.composer-view-*` actions in the Ansicht menu, with automatic unhiding when drafts
     contain non-empty values.
  2. Reply-To input with contact autocompletion and address book search, persisted in drafts and
     exported in outgoing messages and RFC822 EML.
  3. Pre-send validation dialogs (`adw::AlertDialog`) prompting for empty subjects and only-Bcc
     recipients when configured.
  4. Signature formatting respecting top-posting (`composer-top-signature`) and RFC outline
     dashes delimiter `--<br>` (`composer-signature-outline`).
  5. Configurable quoting and forwarding styles (`composer-reply-style`, `composer-forward-style`)
     supporting inline, quoted prefix, blockquote, and attached workflows.
  6. Inline spellchecking toggle (`composer-inline-spelling`) dynamically controlling WebKitGTK.
- Mail preferences (`PreferencesDialog`) provide dedicated controls for mail reading policies:
  1. HTML image loading policy combo row (`mail-image-loading-policy`: `'never'`, `'sometimes'`/`'contacts'`, `'always'`).
  2. Automatic mark-as-read toggle (`mail-mark-seen`) and configurable timeout slider/spinrow (`mail-mark-seen-timeout`).
  3. Caret navigation toggle (`mail-caret-mode`) and magic spacebar toggle (`mail-magic-spacebar`).
  4. Display real absolute timestamp vs relative date toggle (`mail-show-real-date`) and collapsed headers toggle (`mail-headers-collapsed`).
  5. Citation highlighting toggle (`mail-citation-highlighting`) and 5 nested quote level color pickers (`mail-citation-color-1` .. `5`) using `gtk::ColorDialogButton` with live preview color dots and hex subtitles.
- The event editor (`EventDialog`) operates as a non-modal standalone top-level
  `adw::Window` (`modal: false`, no `set_transient_for`), reproducing upstream
  Evolution's `ECompEditor` behavior. It can be moved freely across workspaces and
  monitors without blocking main shell navigation. The right-hand day timeline preview
  hosts all-day events in a dedicated header box above the scrollable 24-hour canvas,
  avoiding overlap with the 00:00–01:00 hour slot, and synchronizes the temporary preview
  chip dynamically with the configured start/end times.


## Verification

- Run the workspace format, check, and test commands from `src/AGENTS.md`.

## Child DOX Index

- No child AGENTS.md files are needed below `dialogs/`.
