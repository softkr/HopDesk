#!/bin/sh
# Builds a debug binary and wraps it in target/debug/HopDesk-Debug.app.
# Usage: scripts/bundle-macos-debug.sh [--install]
#
# Unlike the release bundle, this one keeps verbose logs: every run writes
# ~/Library/Logs/HopDesk/<launcher run>.log and each session's full log lands in its own
# session-*.log there (latest.log points at the newest launcher run).
#
# Set HOPDESK_SIGN_IDENTITY (see scripts/make-signing-cert.sh) to keep the app's identity
# stable across rebuilds, so the Keychain does not ask again for saved passwords; otherwise
# scripts/use-signing-cert.sh picks the local certificate up automatically.
set -eu

cd "$(dirname "$0")/.."

INSTALL=0
while [ $# -gt 0 ]; do
    case "$1" in
        --install) INSTALL=1 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

cargo build
BIN=target/debug/hopdesk
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
APP=target/debug/HopDesk-Debug.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/hopdesk-bin"
cp assets/icon/HopDesk.icns "$APP/Contents/Resources/HopDesk.icns"

cat > "$APP/Contents/MacOS/HopDesk-Debug" <<'WRAPPER'
#!/bin/sh
# Debug launcher: verbose logs go to ~/Library/Logs/HopDesk/. latest.log points at the newest
# launcher run; each session's full log lands in its own session-*.log there. RUST_LOG can
# still be overridden from the environment.
DIR="$(cd "$(dirname "$0")" && pwd)"
LOGDIR="$HOME/Library/Logs/HopDesk"
mkdir -p "$LOGDIR"
LOG="$LOGDIR/debug-$(date +%Y%m%d-%H%M%S).log"
ln -sfn "$LOG" "$LOGDIR/latest.log"
export RUST_LOG="${RUST_LOG:-debug,russh=warn}"
export HOPDESK_LOG_DIR="$LOGDIR"
echo "HopDesk debug run $(date)" >> "$LOG"
exec "$DIR/hopdesk-bin" >>"$LOG" 2>&1
WRAPPER
chmod +x "$APP/Contents/MacOS/HopDesk-Debug"

# A distinct bundle identifier, so the debug app runs beside the release one.
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>HopDesk-Debug</string>
    <key>CFBundleDisplayName</key>
    <string>HopDesk Debug</string>
    <key>CFBundleIdentifier</key>
    <string>app.hopdesk.HopDesk.debug</string>
    <key>CFBundleExecutable</key>
    <string>HopDesk-Debug</string>
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

codesign --force --deep --sign "$(scripts/use-signing-cert.sh)" "$APP"

echo "built $APP"

if [ "$INSTALL" = 1 ]; then
    rm -rf /Applications/HopDesk-Debug.app
    cp -R "$APP" /Applications/
    echo "installed /Applications/HopDesk-Debug.app"
fi
