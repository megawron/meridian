# Rust application source

## Purpose

Own the GTK4/libadwaita application, domain models, dialogs, and backend
service coordination for the Evolution replacement.

## Ownership

- `views/` owns the visible Evolution-like navigation, layout, density, and
  user interaction behavior.
- `services/` owns EDS/Camel/ECal/EBook adapters, portable stores, settings,
  parsing, persistence, and backend-neutral domain operations.
- `dialogs/` owns transient GTK4/libadwaita editors and preferences surfaces.
- `models/` owns GObject list-model objects passed from services to views.
- `window.rs`, `application.rs`, and `style.css` own the application shell,
  global actions, responsive behavior, and shared styling.

## Local Contracts

- GTK4-rs/libadwaita is the only production frontend; Relm4 is not used.
- EDS/Camel/ECal/EBook remain authoritative for synchronized data. Portable
  stores are explicit fallbacks and must not silently replace live EDS data.
- Backend work must not block the GTK main loop. Convert completed worker
  results into GTK models on the main context in bounded chunks when needed.
- Durable user-visible behavior belongs in `ROADMAP.md`; Linux-only build and
  runtime acceptance belongs in `ROADMAP_LINUX.md`.
- User-visible shell actions, primary/transport menu entries, prerequisite
  scopes, and shortcut-help metadata share `action_contract.rs`. Accelerator
  bindings remain executable application data and every documented shortcut
  must resolve through that table; do not add parallel menu/help-only IDs.

## Work Guidance

- Match the original Evolution window structure, navigation, compact density,
  actions, and settings before adding secondary features.
- Keep persistent settings effective in the running window and update all
  affected visible widgets when a setting changes.

## Verification

- Run `cargo fmt --all -- --check` and `cargo check --workspace --locked`.
- Run `cargo test --workspace --locked --no-fail-fast --quiet`; Linux mock and
  EDS runtime checks remain subject to `ROADMAP_LINUX.md`.

## Child DOX Index

- `views/AGENTS.md` — GTK4/libadwaita pages and Evolution interaction parity.
- `services/AGENTS.md` — authoritative backend and portable data services.
- `dialogs/AGENTS.md` — transient editors, preferences, and action flows.
- `models/AGENTS.md` — GObject domain objects used by GTK list models.
