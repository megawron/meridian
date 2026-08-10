#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Helper script to build GIR generator and generate Rust bindings for EDS
# (libcamel, libedataserver, libecal, libebook).
#
# Prerequisites (Fedora 44):
#   sudo dnf install evolution-data-server-devel gobject-introspection-devel

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$SCRIPT_DIR")"
GIR_BUILD_DIR="$WORKSPACE_ROOT/target/gir"
GIR_SPEC_ROOT="$WORKSPACE_ROOT/bindings"

echo "=== Evolution GTK4 — GIR Bindings Generator ==="
echo "Workspace root: $WORKSPACE_ROOT"

# Check required GIR files on system
GIR_FILES=(
    "/usr/share/gir-1.0/Camel-1.2.gir"
    "/usr/share/gir-1.0/EDataServer-1.2.gir"
    "/usr/share/gir-1.0/ECal-2.0.gir"
    "/usr/share/gir-1.0/EBook-1.2.gir"
)

for file in "${GIR_FILES[@]}"; do
    if [[ -f "$file" ]]; then
        echo " [OK] Found $file"
    else
        echo " [WARNING] Missing $file — install evolution-data-server-devel"
    fi
done

for file in "${GIR_FILES[@]}"; do
    if [[ ! -f "$file" ]]; then
        echo "Cannot generate bindings: required GIR files are missing"
        exit 1
    fi
done

echo ""
echo "Step 1: Cloning gtk-rs/gir tool if not present..."
if [[ ! -d "$GIR_BUILD_DIR" ]]; then
    mkdir -p "$GIR_BUILD_DIR"
    git clone https://github.com/gtk-rs/gir.git "$GIR_BUILD_DIR"
fi

echo "Step 2: Building gir binary..."
cd "$GIR_BUILD_DIR"
cargo build --release

echo ""
echo "=== GIR Generator Ready ==="
echo "Gir binary: $GIR_BUILD_DIR/target/release/gir"

echo ""
echo "=== Generating EDS binding sources ==="
for spec in \
    "$GIR_SPEC_ROOT/camel/Gir.toml" \
    "$GIR_SPEC_ROOT/edata-server/Gir.toml" \
    "$GIR_SPEC_ROOT/ecal/Gir.toml" \
    "$GIR_SPEC_ROOT/ebook/Gir.toml"; do
    echo " [RUN] $spec"
    cd "$(dirname "$spec")"
    "$GIR_BUILD_DIR/target/release/gir" -c "$spec"
done

echo "Generated binding sources below the configured target paths."
