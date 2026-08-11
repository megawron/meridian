#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Generate the offline Cargo source manifest used by flatpak-builder.  The
# generator is supplied by flatpak-builder on Linux and is deliberately not
# vendored into this repository.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
generator="${FLATPAK_CARGO_GENERATOR:-}"

if [ -z "$generator" ]; then
    for candidate in \
        /usr/lib/flatpak-builder/flatpak-cargo-generator.py \
        /usr/share/flatpak-builder/flatpak-cargo-generator.py \
        "$root/flatpak-cargo-generator.py"
    do
        if [ -f "$candidate" ]; then
            generator=$candidate
            break
        fi
    done
fi

if [ -z "$generator" ] || [ ! -f "$generator" ]; then
    echo "flatpak-cargo-generator.py nicht gefunden" >&2
    echo "FLATPAK_CARGO_GENERATOR auf den Generator von flatpak-builder setzen" >&2
    exit 1
fi

python3 "$generator" "$root/Cargo.lock" -o "$root/cargo-sources.json"
echo "cargo-sources.json erzeugt. Vor dem Commit scripts/verify-flatpak-manifest.sh ausführen."
