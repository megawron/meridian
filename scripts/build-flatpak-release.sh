#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build and export a single-file distributable Flatpak bundle (.flatpak) for
# Meridian releases.
#
# Usage:
#   ./scripts/build-flatpak-release.sh [--install]

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

install_flag=0
if [ "${1:-}" = "--install" ]; then
    install_flag=1
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
arch=$(flatpak --default-arch 2>/dev/null || uname -m)
bundle="meridian-${version}-${arch}.flatpak"

echo "==> [1/4] Generating offline cargo-sources.json from Cargo.lock..."
./scripts/generate-cargo-sources.sh

echo "==> [2/4] Validating Flatpak manifest..."
./scripts/verify-flatpak-manifest.sh

echo "==> [3/4] Building Flatpak in clean sandbox..."
if [ "$install_flag" -eq 1 ]; then
    flatpak-builder \
        --user \
        --install \
        --force-clean \
        --repo=flatpak-repo \
        flatpak-build-dir \
        com.martingawron.Meridian.json
else
    flatpak-builder \
        --force-clean \
        --repo=flatpak-repo \
        flatpak-build-dir \
        com.martingawron.Meridian.json
fi

echo "==> [4/4] Exporting single-file release bundle: ${bundle}"
flatpak build-bundle \
    flatpak-repo \
    "$bundle" \
    com.martingawron.Meridian \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo

sha256sum "$bundle" > "${bundle}.sha256"
echo "==> Done! Release bundle ready:"
echo "    Bundle:   $root/$bundle"
echo "    Checksum: $root/${bundle}.sha256"
