#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Static release gate for the Flatpak manifest. Catches accidental placeholders,
# malformed JSON, and unpinned external sources before a release build.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
manifest="$root/com.martingawron.Meridian.json"

jq -e '
  .id == "com.martingawron.Meridian"
  and (.modules | length >= 2)
  and ([.modules[].sources[]? | objects | select(.type == "git")] | length >= 1)
  and (all(.modules[].sources[]?; type != "object" or .type != "git" or (.commit | test("^[0-9a-f]{40}$"))))
  and (all(.modules[].sources[]?; type != "object" or .type != "archive" or (.sha256 | test("^[0-9a-f]{64}$"))))
' "$manifest" >/dev/null

if grep -nE 'FIXME|TODO|REPLACE_WITH|PLACEHOLDER' "$manifest"; then
    echo "Flatpak manifest still contains a placeholder" >&2
    exit 1
fi

if [ ! -f "$root/cargo-sources.json" ]; then
    echo "cargo-sources.json is missing; run scripts/generate-cargo-sources.sh first" >&2
    exit 1
fi

jq empty "$root/cargo-sources.json"
echo "Flatpak manifest and cargo-sources.json validated."
