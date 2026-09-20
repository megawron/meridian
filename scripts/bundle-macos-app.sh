#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Bundle Meridian into a proper macOS .app bundle.
# This ensures that macOS TCC (Transparency, Consent, and Control) recognizes
# the app as 'com.martingawron.Meridian' and prompts for Calendar, Contacts, and Reminders
# permissions, allowing the user to manage them in macOS System Settings.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

echo "==> Compiling GSettings schemas..."
glib-compile-schemas data/

echo "==> Building meridian binary with Cargo..."
cargo build --bin meridian

APP_DIR="${ROOT_DIR}/target/Meridian.app"
CONTENTS_DIR="${APP_DIR}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"
SCHEMAS_DIR="${RESOURCES_DIR}/schemas"

echo "==> Assembling ${APP_DIR}..."
rm -rf "${APP_DIR}"
mkdir -p "${MACOS_DIR}" "${SCHEMAS_DIR}"

cp target/debug/meridian "${MACOS_DIR}/meridian"
cp data/Info.plist "${CONTENTS_DIR}/Info.plist"
cp data/gschemas.compiled "${SCHEMAS_DIR}/"
cp data/com.martingawron.Meridian.gschema.xml "${SCHEMAS_DIR}/"

if [ -d data/icons ]; then
    cp -R data/icons "${RESOURCES_DIR}/"
fi
if [ -f data/icons/Meridian.icns ]; then
    cp data/icons/Meridian.icns "${RESOURCES_DIR}/Meridian.icns"
fi

echo "==> Code-signing Meridian.app (ad-hoc)..."
codesign --force --deep -s - "${APP_DIR}"

echo "==> Verifying signature & bundle metadata..."
codesign -dvv "${APP_DIR}"

echo ""
echo "======================================================================"
echo " Meridian.app successfully created at:"
echo "   ${APP_DIR}"
echo "======================================================================"
echo " You can launch it with:"
echo "   open ${APP_DIR}"
echo " or directly in terminal:"
echo "   ${MACOS_DIR}/meridian"
echo "======================================================================"

if [[ "${1:-}" == "--run" ]]; then
    echo "==> Launching Evolution.app..."
    open "${APP_DIR}"
fi
