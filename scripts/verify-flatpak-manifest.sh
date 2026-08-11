#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Static release gate for the Flatpak manifest.  The actual sandbox build is
# intentionally a Linux-only step, but this catches accidental placeholders,
# malformed JSON, and unpinned external sources before a release is attempted.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
manifest="$root/org.gnome.Evolution4.json"

jq -e '
  .id == "org.gnome.Evolution4"
  and (.modules | length >= 2)
  and ([.modules[].sources[]? | objects | select(.type == "git")] | length >= 1)
  and (all(.modules[].sources[]?; type != "object" or .type != "git" or (.commit | test("^[0-9a-f]{40}$"))))
  and (all(.modules[].sources[]?; type != "object" or .type != "archive" or (.sha256 | test("^[0-9a-f]{64}$"))))
' "$manifest" >/dev/null

if grep -nE 'FIXME|TODO|REPLACE_WITH|PLACEHOLDER' "$manifest"; then
    echo "Flatpak-Manifest enthält noch einen Platzhalter" >&2
    exit 1
fi

if [ ! -f "$root/cargo-sources.json" ]; then
    echo "cargo-sources.json fehlt; unter Linux mit scripts/generate-cargo-sources.sh erzeugen" >&2
    exit 1
fi

jq empty "$root/cargo-sources.json"
echo "Flatpak-Manifest und cargo-sources.json sind statisch validiert."
