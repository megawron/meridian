# Contributing to Meridian

Thank you for your interest in Meridian. Since this project has been developed solo so far, any help with testing, bug reports, or code contributions is greatly appreciated.

## Be Kind

Please stay friendly, patient, and constructive when opening issues or discussing changes. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Reporting Bugs & Feedback

My own daily testing mainly covers Fedora on GNOME with Exchange and Google accounts. If you use a different Linux distribution, desktop environment, or mail and calendar provider, your feedback is especially helpful.

When opening an issue, please include:
1. What you did and what went wrong.
2. Your distribution and desktop environment.
3. The account type involved (e.g. IMAP, CalDAV/CardDAV, Exchange, Google, local folders).
4. Terminal output or logs if available (you can run `RUST_LOG=debug cargo run` for more details).

## Code Contributions

1. Fork the repository and create a branch for your change.
2. Keep changes focused on a single fix or feature per pull request.
3. Make sure the project builds cleanly and tests pass before submitting:
   ```bash
   cargo fmt --check
   cargo check
   cargo test
   ```
4. If you are planning a larger change or UI rework, please open an issue first so we can discuss the approach before you invest a lot of time.

## License

By contributing to Meridian, you agree that your contributions will be licensed under the [GPL-3.0-or-later](LICENSE).
