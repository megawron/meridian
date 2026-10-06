# Meridian — Agent Work Guidelines

See [AGENT.md](AGENT.md) for the complete design system, GNOME HIG/Libadwaita rules, and coding standards.

## Quick Summary for AI Agents
1. **GNOME HIG & Libadwaita**: Always inspect `../libadwaita/demo` for reference patterns. Do not write custom CSS that breaks or shrinks Libadwaita styles.
2. **Context Menus**: Strict standard across all views (min-height: 32px, padding: 0 12px, border-radius: 9px, standard typography, 16px symbolic icons).
3. **100% US-English**: All code, comments, and `gettext` strings in US-English. Localizations in `po/`.
4. **Privacy**: Never reference real `.eml` files or local user paths. Use synthetic fixtures.
5. **Simplicity**: Keep documentation simple, actionable, and free of unnecessary overhead.
