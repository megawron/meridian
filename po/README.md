# Translating Meridian (Localization Guide)

Thank you for helping translate **Meridian**! You do **not** need programming experience or knowledge of Rust to contribute translations.

Meridian uses **GNU gettext** (`.po` and `.pot` files), the official standard across the GNOME ecosystem. This means you can use user-friendly graphical translation editors such as **Poedit**, **GNOME Translation Editor (Gtranslator)**, or **Lokalize**.

---

## Supported Languages (`po/LINGUAS`)

Currently maintained translation catalogs in `po/`:
- `de` — German (*Deutsch*)
- `es` — Spanish (*Español*)
- `fr` — French (*Français*)
- `ko` — Korean (*한국어*)
- `zh_CN` — Chinese, Simplified (*简体中文*)

The source language in the codebase is **US English (`en_US`)**.

---

## Option 1: Improving an Existing Translation (No Terminal Required)

1. Install a graphical PO editor:
   - **[Poedit](https://poedit.net/)** (Available on Linux/Flathub, Windows, and macOS)
   - **[GNOME Translation Editor (Gtranslator)](https://flathub.org/apps/org.gnome.Gtranslator)** (Flathub / Linux)
   - **[Lokalize](https://apps.kde.org/lokalize/)** (Flathub / Linux)
2. Open the `.po` file for your language from the `po/` folder (for example `po/de.po`, `po/es.po`, `po/fr.po`, `po/ko.po`, or `po/zh_CN.po`).
3. Translate any untranslated items or refine existing translations.
   - **Placeholders**: Keep placeholders like `{}`, `{n}`, `{name}`, `{count}`, or `%s` intact in your translation! For example:
     - Source: `Account “{name}” enabled`
     - German: `Konto „{name}“ aktiviert`
   - **Ellipsis**: If the English string ends with `…` (Unicode ellipsis), keep `…` at the end of the translated string as well.
   - **Keyboard Accelerators**: If a string contains an underscore mnemonic (such as `_Cancel`), place `_` before the corresponding shortcut letter in your language.
4. Save the `.po` file in your editor and open a Pull Request (or attach the updated `.po` file to a GitHub Issue if you are not familiar with Git).

---

## Option 2: Adding a New Language

1. Determine the standard ISO language code (for example `it` for Italian, `pt_BR` for Brazilian Portuguese, `ja` for Japanese, `nl` for Dutch, `pl` for Polish).
2. Add the language code in alphabetical order to `po/LINGUAS`.
3. Create the new `.po` file from the template `po/meridian.pot`:
   - **Using Poedit**: Open Poedit → *Create New Translation* → select `po/meridian.pot` → choose your language → save as `po/<lang>.po`.
   - **Or via terminal**:
     ```bash
     msginit --input=po/meridian.pot --locale=<lang> --output-file=po/<lang>.po --no-translator
     ```
4. Translate the strings and save `po/<lang>.po`.

---

## Updating the Translation Template (`meridian.pot`) & Syncing `.po` Files

When new UI strings are added to the codebase, maintainers or contributors can refresh `po/meridian.pot` and merge changes into all `po/*.po` files using either **Meson** (standard GNOME workflow) or the standalone helper script:

### Using the helper script:
```bash
python3 scripts/update_po.py
```

### Or using Meson (GNOME Damned Lies / standard workflow):
```bash
meson setup builddir
meson compile -C builddir meridian-pot
meson compile -C builddir meridian-update-po
```

### Validating `.po` files:
To check that all `.po` files have valid syntax, headers, and format placeholders:
```bash
for po in po/*.po; do
  echo "Checking $po..."
  msgfmt --check --statistics -o /dev/null "$po"
done
```

### Testing a translation locally:
When you run `cargo build` or `cargo run`, Meridian's build script (`src/build.rs`) automatically compiles all `po/*.po` files into `.mo` catalogs. You can launch Meridian in any supported language by setting `LANGUAGE` and `LC_ALL`:
```bash
LANGUAGE=de LC_ALL=de_DE.UTF-8 cargo run
LANGUAGE=es LC_ALL=es_ES.UTF-8 cargo run
LANGUAGE=fr LC_ALL=fr_FR.UTF-8 cargo run
LANGUAGE=ko LC_ALL=ko_KR.UTF-8 cargo run
LANGUAGE=zh_CN LC_ALL=zh_CN.UTF-8 cargo run
```
