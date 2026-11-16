#!/bin/sh
# Renders assets/icon/*.svg (HopDesk icon set v1.0) into every icon the builds use:
#   HopDesk.icns          macOS app bundle
#   icon-256.png          macOS window/Dock icon (on Apple's icon grid, with margin and shadow)
#   icon-session-256.png  macOS Dock icon of a session window (dark variant, same grid)
#   icon-tray-44.png      macOS menu bar icon (template image, 22 pt @2x; the 24-unit grid's
#                         margin leaves the 18 pt glyph Apple recommends)
#   HopDesk.ico           Windows exe icon (embedded by build.rs; also the tray icon)
#   icon-full-256.png     Windows/Linux window icon and Linux desktop icon (tile fills the square)
# Sources: icon.svg (primary), icon-small.svg (simplified, used up to 32 px), icon-dark.svg,
# icon-tray.svg. The macOS variants are the same artwork scaled onto Apple's 824 px grid.
# Needs the `resvg` CLI (cargo install resvg) and macOS `iconutil`.
set -eu

cd "$(dirname "$0")/../assets/icon"
command -v resvg >/dev/null || { echo "resvg not found: cargo install resvg" >&2; exit 1; }

TMP=$(mktemp -d)
SET=$TMP/HopDesk.iconset
mkdir -p "$SET" "$TMP/ico"
render() { resvg -w "$2" -h "$2" "$1" "$3"; }

# Apple's grid: the tile is 824/1024 of the canvas, slightly above center, with a drop shadow
# (the shadow filter is defined in the full-size sources; the small one is drawn without it).
grid() { # source, translate, scale, filter attribute, output
    python3 - "$@" <<'PY'
import sys, pathlib
src, translate, scale, extra, out = sys.argv[1:6]
svg = pathlib.Path(src).read_text()
head, sep, body = svg.partition("</defs>")
if not sep:
    head, sep, body = svg.partition(">")  # no <defs>: wrap after the <svg> tag
body, close, tail = body.rpartition("</svg>")
wrap = f'<g transform="translate({translate}) scale({scale})"{extra}>'
pathlib.Path(out).write_text(head + sep + wrap + body + "</g>" + close + tail)
PY
}
shadow=' filter="url(#bodyshadow)"'
grid icon.svg "100 92" 0.8046875 "$shadow" "$TMP/macos.svg"
grid icon-dark.svg "100 92" 0.8046875 "$shadow" "$TMP/macos-dark.svg"
grid icon-small.svg "6.25 5.75" 0.8046875 "" "$TMP/macos-small.svg"

# macOS
render "$TMP/macos-small.svg" 16   "$SET/icon_16x16.png"
render "$TMP/macos-small.svg" 32   "$SET/icon_16x16@2x.png"
render "$TMP/macos-small.svg" 32   "$SET/icon_32x32.png"
render "$TMP/macos.svg"       64   "$SET/icon_32x32@2x.png"
render "$TMP/macos.svg"       128  "$SET/icon_128x128.png"
render "$TMP/macos.svg"       256  "$SET/icon_128x128@2x.png"
render "$TMP/macos.svg"       256  "$SET/icon_256x256.png"
render "$TMP/macos.svg"       512  "$SET/icon_256x256@2x.png"
render "$TMP/macos.svg"       512  "$SET/icon_512x512.png"
render "$TMP/macos.svg"       1024 "$SET/icon_512x512@2x.png"
iconutil -c icns "$SET" -o HopDesk.icns
render "$TMP/macos.svg" 256 icon-256.png
# macOS Dock icon of a running session (tells it apart from the launcher)
render "$TMP/macos-dark.svg" 256 icon-session-256.png
# macOS menu bar icon of the launcher (black on transparent; macOS tints it)
render icon-tray.svg 44 icon-tray-44.png

# Windows / Linux: the tile fills the square.
for n in 16 24 32; do render icon-small.svg $n "$TMP/ico/$n.png"; done
for n in 48 64 128 256; do render icon.svg $n "$TMP/ico/$n.png"; done
cp "$TMP/ico/256.png" icon-full-256.png

# ICO with PNG-compressed entries (supported since Windows Vista).
python3 - "$TMP/ico" HopDesk.ico <<'PY'
import struct, sys, pathlib
src, out = pathlib.Path(sys.argv[1]), sys.argv[2]
sizes = [16, 24, 32, 48, 64, 128, 256]
data = [(src / f"{n}.png").read_bytes() for n in sizes]
offset = 6 + 16 * len(sizes)
header = struct.pack("<HHH", 0, 1, len(sizes))
entries = b""
for n, d in zip(sizes, data):
    entries += struct.pack("<BBBBHHII", n % 256, n % 256, 0, 0, 1, 32, len(d), offset)
    offset += len(d)
pathlib.Path(out).write_bytes(header + entries + b"".join(data))
PY

rm -rf "$TMP"
echo "wrote HopDesk.icns, HopDesk.ico, icon-256.png, icon-session-256.png, icon-tray-44.png, icon-full-256.png in assets/icon"
