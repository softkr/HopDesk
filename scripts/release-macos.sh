#!/bin/sh
# Builds HopDesk.app and packages it as a drag-to-install disk image:
#   dist/HopDesk-<version>-macos-<arch>.dmg
# Usage: scripts/release-macos.sh [--universal]   (arch is "universal" with --universal)
set -eu

cd "$(dirname "$0")/.."
scripts/bundle-macos.sh "$@"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
ARCH=$(uname -m)
[ "${1:-}" = "--universal" ] && ARCH=universal
mkdir -p dist
DMG="dist/HopDesk-${VERSION}-macos-${ARCH}.dmg"

STAGE=$(mktemp -d)
cp -R target/release/HopDesk.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -volname "HopDesk ${VERSION}" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
rm -rf "$STAGE"

shasum -a 256 "$DMG"
