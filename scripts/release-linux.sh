#!/bin/sh
# Builds a release binary and packages it as dist/HopDesk-<version>-linux-<arch>.tar.gz
# (binary, desktop entry, icon, install.sh).
set -eu

cd "$(dirname "$0")/.."
cargo build --release

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
ARCH=$(uname -m)
NAME="HopDesk-${VERSION}-linux-${ARCH}"
STAGE=$(mktemp -d)/$NAME

mkdir -p "$STAGE" dist
cp target/release/hopdesk "$STAGE/"
cp packaging/linux/hopdesk.desktop packaging/linux/install.sh "$STAGE/"
cp assets/icon/icon-full-256.png "$STAGE/hopdesk.png"
chmod +x "$STAGE/hopdesk" "$STAGE/install.sh"

tar -C "$(dirname "$STAGE")" -czf "dist/$NAME.tar.gz" "$NAME"
rm -rf "$(dirname "$STAGE")"
sha256sum "dist/$NAME.tar.gz" 2>/dev/null || shasum -a 256 "dist/$NAME.tar.gz"
