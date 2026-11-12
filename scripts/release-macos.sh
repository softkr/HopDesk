#!/bin/sh
# Builds HopDesk.app and packages it as a drag-to-install disk image:
#   dist/HopDesk-<version>-macos-<arch>.dmg
# Usage: scripts/release-macos.sh [--universal | --target <triple>]
#   <arch> is "universal", "apple-silicon" (aarch64) or "intel" (x86_64)
set -eu

cd "$(dirname "$0")/.."
scripts/bundle-macos.sh "$@"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
case "${1:-} ${2:-}" in
    "--universal "*) ARCH=universal ;;
    "--target aarch64-"*) ARCH=apple-silicon ;;
    "--target x86_64-"*) ARCH=intel ;;
    *) case "$(uname -m)" in arm64) ARCH=apple-silicon ;; x86_64) ARCH=intel ;; *) ARCH=$(uname -m) ;; esac ;;
esac
mkdir -p dist
DMG="dist/HopDesk-${VERSION}-macos-${ARCH}.dmg"

STAGE=$(mktemp -d)
cp -R target/release/HopDesk.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -volname "HopDesk ${VERSION}" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
rm -rf "$STAGE"

shasum -a 256 "$DMG"
