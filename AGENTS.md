# Meridian — Agent Work Guidelines

See [DESIGN.md](DESIGN.md) for the canonical visual design system (cards, shadows, borders, typography, menus) and [AGENT.md](AGENT.md) for GNOME HIG/Libadwaita rules and coding standards.

## Quick Summary for AI Agents
1. **GNOME HIG & Unified Design System ([DESIGN.md](DESIGN.md))**: Always inspect `../libadwaita/demo` for reference patterns. All cards (`.card`, `list.boxed-list`, `.composer-header-card`, Calendar composer groups, `.mail-preview-card`, `.task-boxed-list`) use Libadwaita's exact `%card` styling (`border: none`, `box-shadow: 0 0 0 1px RGB(0 0 6 / 3%), 0 1px 3px 1px RGB(0 0 6 / 7%), 0 2px 6px 2px RGB(0 0 6 / 3%)`) on `var(--window-bg-color)` (`#fafafb`) with `1px solid var(--card-shade-color)` internal row dividers. Never add double borders to `GtkPaned` children.
2. **Context Menus**: Strict standard across all views (min-height: 30px, padding: 0 8px, border-radius: 7px, standard typography, no icons for verb actions).
3. **100% US-English**: All code, comments, and `gettext` strings in US-English. Localizations in `po/`.
4. **Privacy**: Never reference real `.eml` files or local user paths. Use synthetic fixtures.
5. **Simplicity**: Keep documentation simple, actionable, and free of unnecessary overhead.
