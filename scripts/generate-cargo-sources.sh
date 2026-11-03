#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Generate the offline Cargo source manifest (cargo-sources.json) directly from
# Cargo.lock for flatpak-builder offline sandbox builds.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

python3 - "$root/Cargo.lock" "$root/cargo-sources.json" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

lock_path = Path(sys.argv[1])
out_path = Path(sys.argv[2])

data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
sources = []

for pkg in data.get("package", []):
    name = pkg.get("name")
    version = pkg.get("version")
    source = pkg.get("source", "")
    checksum = pkg.get("checksum")
    if not source.startswith("registry+") or not checksum:
        continue
    crate_dir = f"cargo/vendor/{name}-{version}"
    sources.append({
        "type": "archive",
        "archive-type": "tar-gzip",
        "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        "sha256": checksum,
        "dest": crate_dir,
    })
    sources.append({
        "type": "inline",
        "contents": json.dumps({"package": checksum, "files": {}}),
        "dest": crate_dir,
        "dest-filename": ".cargo-checksum.json",
    })

sources.append({
    "type": "inline",
    "contents": (
        "[source.crates-io]\n"
        'replace-with = "vendored-sources"\n\n'
        "[source.vendored-sources]\n"
        'directory = "cargo/vendor"\n'
    ),
    "dest": "cargo",
    "dest-filename": "config.toml",
})

out_path.write_text(json.dumps(sources, indent=2) + "\n", encoding="utf-8")
print(f"Generated {out_path} ({len(sources)} entries).")
PY
