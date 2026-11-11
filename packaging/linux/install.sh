#!/bin/sh
# Installs HopDesk for the current user (no root needed):
#   ~/.local/bin/hopdesk, desktop entry and icon (shows up in the app menu).
# Remove with: ./install.sh --uninstall
set -eu

cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/hopdesk" "$APPS/hopdesk.desktop" "$ICONS/hopdesk.png"
    echo "HopDesk removed. Settings remain in ~/.config/hopdesk."
    exit 0
fi

mkdir -p "$BIN" "$APPS" "$ICONS"
install -m 755 hopdesk "$BIN/hopdesk"
install -m 644 hopdesk.png "$ICONS/hopdesk.png"
sed "s|^Exec=hopdesk|Exec=$BIN/hopdesk|" hopdesk.desktop > "$APPS/hopdesk.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" 2>/dev/null || true
echo "HopDesk installed: $BIN/hopdesk (also in the app menu)"
