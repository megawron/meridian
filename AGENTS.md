# DOX framework

- DOX is highly performant AGENTS.md hierarchy installed here
- Agent must follow DOX instructions across any edits

## Core Contract

- AGENTS.md files are binding work contracts for their subtrees
- Work products, source materials, instructions, records, assets, and durable docs must stay understandable from the nearest applicable AGENTS.md plus every parent AGENTS.md above it

## Read Before Editing

1. Read the root AGENTS.md
2. Identify every file or folder you expect to touch
3. Walk from the repository root to each target path
4. Read every AGENTS.md found along each route
5. If a parent AGENTS.md lists a child AGENTS.md whose scope contains the path, read that child and continue from there
6. Use the nearest AGENTS.md as the local contract and parent docs for repo-wide rules
7. If docs conflict, the closer doc controls local work details, but no child doc may weaken DOX

Do not rely on memory. Re-read the applicable DOX chain in the current session before editing.

## Update After Editing

Every meaningful change requires a DOX pass before the task is done.

Update the closest owning AGENTS.md when a change affects:

- purpose, scope, ownership, or responsibilities
- durable structure, contracts, workflows, or operating rules
- required inputs, outputs, permissions, constraints, side effects, or artifacts
- user preferences about behavior, communication, process, organization, or quality
- AGENTS.md creation, deletion, move, rename, or index contents

Update parent docs when parent-level structure, ownership, workflow, or child index changes. Update child docs when parent changes alter local rules. Remove stale or contradictory text immediately. Small edits that do not change behavior or contracts may leave docs unchanged, but the DOX pass still must happen.

## Hierarchy

- Root AGENTS.md is the DOX rail: project-wide instructions, global preferences, durable workflow rules, and the top-level Child DOX Index
- Child AGENTS.md files own domain-specific instructions and their own Child DOX Index
- Each parent explains what its direct children cover and what stays owned by the parent
- The closer a doc is to the work, the more specific and practical it must be

## Child Doc Shape

- Create a child AGENTS.md when a folder becomes a durable boundary with its own purpose, rules, responsibilities, workflow, materials, or quality standards
- Work Guidance must reflect the current standards of the project or user instructions; if there are no specific standards or instructions yet, leave it empty
- Verification must reflect an existing check; if no verification framework exists yet, leave it empty and update it when one exists

Default section order:
- Purpose
- Ownership
- Local Contracts
- Work Guidance
- Verification
- Child DOX Index

## Style

- Keep docs concise, current, and operational
- Document stable contracts, not diary entries
- Put broad rules in parent docs and concrete details in child docs
- Prefer direct bullets with explicit names
- Do not duplicate rules across many files unless each scope needs a local version
- Delete stale notes instead of explaining history
- Trim obvious statements, repeated rules, misplaced detail, and warnings for risks that no longer exist

## Closeout

1. Re-check changed paths against the DOX chain
2. Update nearest owning docs and any affected parents or children
3. Refresh every affected Child DOX Index
4. Remove stale or contradictory text
5. Run existing verification when relevant
6. Report any docs intentionally left unchanged and why

## User Preferences

When the user requests a durable behavior change, record it here or in the relevant child AGENTS.md

- GTK4-rs/libadwaita is the only production frontend. Reproduce the original
  Evolution application as closely as possible in window structure,
  navigation, layout, actions, and settings. Use the clean upstream checkout
  at `/Users/martin/Documents/Projects/evolution-gtk4/evolution-upstream-reference`
  as the read-only authority for original action names, menus, shortcuts,
  settings and behavior; never edit or commit in that reference clone while
  implementing this project. Keep the original EDS/Camel/ECal/EBook services
  authoritative and make portable UI choices persistent where the original
  application exposes them.
- Prioritize daily mail workflows, responsiveness, data integrity, recovery,
  and stability before secondary parity work. A visible control is not a
  feature until its operation, persistence, error path, and tests are wired.
- `main` is the only writable development branch. The separate upstream clone
  is read-only reference material; do not create or maintain parallel product
  feature branches unless the user explicitly changes this policy.

## Roadmap Workflow

- `ROADMAP.md` is the canonical product scope and evidence-backed status.
- `ROADMAP_EXECUTION.md` is the atomic, prioritized work queue. Select work
  from it and split any item that cannot end in one coherent, testable commit.
- `ROADMAP_LINUX.md` owns work and acceptance that requires Linux, EDS, GOA,
  providers, WebKitGTK runtime, desktop integration, or packaging.
- Never mark an umbrella item complete from a narrow test. Update status only
  when every acceptance criterion has direct evidence.
- Keep Linux-only handoffs executable: include the commit, setup, exact action,
  expected result, negative case, and evidence to record.
- When parallel agents are available, assign disjoint write sets from the
  execution roadmap. Keep one writer per path, let the main agent own the
  critical path, and integrate each result separately through review and
  verification. Agent worktrees are temporary implementation machinery, not
  additional product branches; `main` remains the only maintained branch.
- The agent-manager workflow is explicit: the main agent coordinates the queue,
  assigns Luna-Max subagents disjoint implementation paths, reviews each
  report and diff, runs the relevant verification, and is the only agent that
  integrates, commits, or pushes. Luna-Max subagents stay within their assigned
  paths and do not edit roadmaps/DOX files or commit/push on their own.
- A portable green test run is evidence for the portable slice only. Keep
  independent review, Linux, visual, EDS, provider, and desktop acceptance
  visibly open until their direct evidence exists; do not convert these rests
  into `DONE` or completion percentages.

## Child DOX Index

- `src/AGENTS.md` owns the Rust application source and indexes its UI, service,
  dialog, and model boundaries.
- `ROADMAP.md` owns complete product scope and verified parity status.
- `ROADMAP_EXECUTION.md` owns atomic priorities and acceptance criteria.
- `ROADMAP_LINUX.md` owns Linux-/EDS-only implementation and acceptance.
- `docs/UPSTREAM_GROUPWARE_PARITY_AUDIT.md` owns the static GTK3↔GTK4
  groupware evidence matrix, portable package cuts, and Linux boundaries.
- `docs/UPSTREAM_MAIL_SHELL_PARITY_AUDIT.md` owns the static upstream
  mail/shell action matrix and the strongest next mail-surface packages.
- `docs/UPSTREAM_SETTINGS_PARITY_AUDIT.md` owns the upstream-to-GTK4 settings
  key inventory, preference/live/backup/migration evidence, and atomic
  settings package cuts.
- Other root-owned files include `README.md`, `LICENSE`, `banner.jpg`,
  `video-thumbnail.jpg`, and root-level project documentation.
