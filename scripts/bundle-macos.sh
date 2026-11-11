#!/bin/sh
# Builds a release binary and wraps it in target/release/HopDesk.app.
# Usage: scripts/bundle-macos.sh [--universal] [--install]
#   --universal  one binary for Apple Silicon and Intel
#                (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin)
#   --install    copy the app to /Applications
set -eu

cd "$(dirname "$0")/.."

UNIVERSAL=0
INSTALL=0
for arg in "$@"; do
    case "$arg" in
        --universal) UNIVERSAL=1 ;;
        --install) INSTALL=1 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ "$UNIVERSAL" = 1 ]; then
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    lipo -create -output target/release/hopdesk \
        target/aarch64-apple-darwin/release/hopdesk target/x86_64-apple-darwin/release/hopdesk
else
    cargo build --release
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=target/release/HopDesk.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/hopdesk "$APP/Contents/MacOS/hopdesk"
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

# Ad-hoc signature so Gatekeeper and the Keychain treat the bundle as one app.
codesign --force --deep --sign - "$APP"

echo "built $APP"

if [ "$INSTALL" = 1 ]; then
    rm -rf /Applications/HopDesk.app
    cp -R "$APP" /Applications/
    echo "installed /Applications/HopDesk.app"
fi
