# Contributing to Meridian

Thank you for your interest in Meridian. Since this project has been developed solo so far, any help with testing, bug reports, or code contributions is greatly appreciated.

## Be Kind

Please stay friendly, patient, and constructive when opening issues or discussing changes. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Security Issues

Please **do not** report security vulnerabilities in public GitHub issues. Instead, report them confidentially via [GitHub Private Vulnerability Reporting](https://github.com/megawron/meridian/security/advisories/new) or by email to [martin@martingawron.com](mailto:martin@martingawron.com). See [SECURITY.md](SECURITY.md) for details.

## Reporting Bugs & Feedback

My own daily testing only covers Fedora/GNOME with Exchange and Google accounts. Testing Meridian across other Linux distributions, desktop environments, and mail or calendar providers is a collaborative community effort.

Before opening an issue, **please make sure you are running the latest version of Meridian**. Issues reported against outdated versions will be closed.

When opening an issue, please include:
1. **What happened**: What you did, what you expected, and what went wrong.
2. **Meridian version**: Latest release version or Git commit hash.
3. **Distribution & version**: e.g. Fedora 41, Ubuntu 24.10, Arch Linux.
4. **Desktop environment & version**: e.g. GNOME 47, KDE Plasma 6.2, Sway.
5. **Evolution Data Server (EDS) version**: Output of `pkg-config --modversion libedataserver-1.2` (or package version).
6. **Account provider / protocol**: e.g. generic IMAP, CalDAV/CardDAV, Exchange/EWS, Google, local folders.
7. **Logs / terminal output**: Run `RUST_LOG=debug cargo run` and attach the relevant output if possible.

## Translating Meridian (No Coding Required)

Meridian follows the official GNOME internationalization (i18n) standards using GNU `gettext` (`po/*.po` and `po/meridian.pot`). Non-technical contributors are warmly welcome to improve existing translations or add new languages!

- **Graphical Editors**: You can edit files in `po/` (such as `po/de.po`, `po/es.po`, `po/fr.po`, `po/ko.po`, `po/zh_CN.po`) or create a new language from `po/meridian.pot` using **[Poedit](https://poedit.net/)**, **[GNOME Translation Editor (Gtranslator)](https://flathub.org/apps/org.gnome.Gtranslator)**, or **[Lokalize](https://apps.kde.org/lokalize/)**.
- **Adding a Language**: Open `po/meridian.pot` in Poedit, save your translation as `po/<lang>.po`, and add `<lang>` alphabetically to `po/LINGUAS`.
- **Submitting Without Git**: If you are not comfortable with Git or Pull Requests, you can simply attach your updated `.po` file to a [GitHub Issue](https://github.com/megawron/meridian/issues).
- For full details, placeholder rules, and testing instructions, see the **[Localization Guide (`po/README.md`)](po/README.md)**.

## Code Contributions

1. Fork the repository and create a branch for your change.
2. Keep changes focused on a single fix or feature per pull request.
3. All code, comments, and user-facing `gettext("...")` source strings must be in **US English**.
4. Make sure the project builds cleanly and tests pass before submitting:
   ```bash
   cargo fmt --check
   cargo check
   cargo test
   ```
5. If you are planning a larger change or UI rework, please open an issue first so we can discuss the approach before you invest a lot of time.

## License

By contributing to Meridian, you agree that your contributions will be licensed under the [GPL-3.0-or-later](LICENSE).
