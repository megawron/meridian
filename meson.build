# Meridian — Meson Build System
# This is the GNOME-standard build system. While the primary build
# is Cargo-based, Meson handles resource compilation, .desktop file
# installation, icon installation, and Flatpak integration.

project(
  'meridian',
  'rust',
  version: '0.1.0',
  license: 'GPL-3.0-or-later',
  meson_version: '>= 0.64.0',
)

# ── Dependencies ─────────────────────────────────────────────────────────
dependency('gtk4', version: '>= 4.16')
dependency('libadwaita-1', version: '>= 1.6')
dependency('webkitgtk-6.0', version: '>= 2.44')
dependency('camel-1.2', version: '>= 3.54')
dependency('libedataserver-1.2', version: '>= 3.54')

# ── Application metadata ────────────────────────────────────────────────
application_id = 'com.martingawron.Meridian'
gettext_package = meson.project_name()

# ── Directories ──────────────────────────────────────────────────────────
prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
localedir = prefix / get_option('localedir')
iconsdir = datadir / 'icons'

# ── Subdirectories ───────────────────────────────────────────────────────
i18n = import('i18n')
subdir('data')
subdir('po')

# ── Cargo build integration ──────────────────────────────────────────────
cargo = find_program('cargo')
sh = find_program('sh')
cargo_env = environment()
cargo_env.set('CARGO_HOME', meson.project_build_root() / 'cargo-home')

if get_option('buildtype') == 'release'
  cargo_profile = 'release'
else
  cargo_profile = 'debug'
endif

cargo_build = custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: bindir,
  env: cargo_env,
  command: [
    sh,
    meson.project_source_root() / 'scripts' / 'build-cargo.sh',
    cargo,
    cargo_profile,
    meson.project_source_root(),
    meson.project_build_root() / 'target',
    '@OUTPUT@',
  ],
)
