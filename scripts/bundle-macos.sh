#!/bin/sh
# Builds a release binary and wraps it in target/release/HopDesk.app.
# Usage: scripts/bundle-macos.sh [--universal | --target <triple>] [--install]
#   --universal        one binary for Apple Silicon and Intel
#                      (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin)
#   --target <triple>  build for one architecture, e.g. x86_64-apple-darwin (Intel)
#   --install          copy the app to /Applications
set -eu

cd "$(dirname "$0")/.."

UNIVERSAL=0
INSTALL=0
TARGET=
while [ $# -gt 0 ]; do
    case "$1" in
        --universal) UNIVERSAL=1 ;;
        --install) INSTALL=1 ;;
        --target) TARGET=${2:?--target needs a triple}; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

# Cargo's own outputs are never overwritten, so a later native build is not mistaken for current.
if [ -n "$TARGET" ]; then
    cargo build --release --target "$TARGET"
    BIN="target/$TARGET/release/hopdesk"
elif [ "$UNIVERSAL" = 1 ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    mkdir -p target/universal
    BIN=target/universal/hopdesk
    lipo -create -output "$BIN" \
        target/aarch64-apple-darwin/release/hopdesk target/x86_64-apple-darwin/release/hopdesk
else
    cargo build --release
    BIN=target/release/hopdesk
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=target/release/HopDesk.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/hopdesk"
cp assets/icon/HopDesk.icns "$APP/Contents/Resources/HopDesk.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>HopDesk</string>
    <key>CFBundleDisplayName</key>
    <string>HopDesk</string>
    <key>CFBundleIdentifier</key>
    <string>app.hopdesk.HopDesk</string>
    <key>CFBundleExecutable</key>
    <string>hopdesk</string>
    <key>CFBundleIconFile</key>
    <string>HopDesk</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

# HOPDESK_SIGN_IDENTITY names the certificate from scripts/make-signing-cert.sh (in a keychain
# on the search list). Releases must use it: the Keychain trusts saved passwords to the
# signing certificate, so updates keep access. Without it the bundle is signed ad hoc, and
# the Keychain asks again after every rebuild. scripts/use-signing-cert.sh picks the identity
# up (and prepares the keychain once) when the local certificate exists.
IDENTITY=$(scripts/use-signing-cert.sh)
codesign --force --deep --sign "${IDENTITY:--}" "$APP"

echo "built $APP"

if [ "$INSTALL" = 1 ]; then
    rm -rf /Applications/HopDesk.app
    cp -R "$APP" /Applications/
    echo "installed /Applications/HopDesk.app"
fi
