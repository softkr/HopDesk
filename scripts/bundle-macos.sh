#!/bin/sh
# Builds a release binary and wraps it in target/release/HopDesk.app.
# Usage: scripts/bundle-macos.sh [--install]   (--install copies the app to /Applications)
set -eu

cd "$(dirname "$0")/.."
cargo build --release

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=target/release/HopDesk.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/hopdesk "$APP/Contents/MacOS/hopdesk"

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
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

# Ad-hoc signature so Gatekeeper and the Keychain treat the bundle as one app.
codesign --force --deep --sign - "$APP"

echo "built $APP"

if [ "${1:-}" = "--install" ]; then
    rm -rf /Applications/HopDesk.app
    cp -R "$APP" /Applications/
    echo "installed /Applications/HopDesk.app"
fi
