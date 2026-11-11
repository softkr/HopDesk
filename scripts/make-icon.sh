#!/bin/sh
# Renders assets/icon/*.svg into every icon the builds use:
#   HopDesk.icns        macOS app bundle
#   icon-256.png        macOS window/Dock icon (keeps Apple's icon-grid margin)
#   HopDesk.ico         Windows exe icon (embedded by build.rs)
#   icon-full-256.png   Windows/Linux window icon and Linux desktop icon (margin cropped)
# Needs the `resvg` CLI (cargo install resvg) and macOS `iconutil`.
# icon-small.svg has heavier strokes and is used for sizes up to 32 px.
set -eu

cd "$(dirname "$0")/../assets/icon"
command -v resvg >/dev/null || { echo "resvg not found: cargo install resvg" >&2; exit 1; }

TMP=$(mktemp -d)
SET=$TMP/HopDesk.iconset
mkdir -p "$SET" "$TMP/ico"
render() { resvg -w "$2" -h "$2" "$1" "$3"; }

# macOS
render icon-small.svg 16   "$SET/icon_16x16.png"
render icon-small.svg 32   "$SET/icon_16x16@2x.png"
render icon-small.svg 32   "$SET/icon_32x32.png"
render icon.svg       64   "$SET/icon_32x32@2x.png"
render icon.svg       128  "$SET/icon_128x128.png"
render icon.svg       256  "$SET/icon_128x128@2x.png"
render icon.svg       256  "$SET/icon_256x256.png"
render icon.svg       512  "$SET/icon_256x256@2x.png"
render icon.svg       512  "$SET/icon_512x512.png"
render icon.svg       1024 "$SET/icon_512x512@2x.png"
iconutil -c icns "$SET" -o HopDesk.icns
render icon.svg 256 icon-256.png

# Windows / Linux: crop the macOS grid margin so the tile fills the square.
crop='s/viewBox="0 0 1024 1024"/viewBox="88 88 848 848"/'
sed "$crop" icon.svg > "$TMP/full.svg"
sed "$crop" icon-small.svg > "$TMP/full-small.svg"
for n in 16 24 32; do render "$TMP/full-small.svg" $n "$TMP/ico/$n.png"; done
for n in 48 64 128 256; do render "$TMP/full.svg" $n "$TMP/ico/$n.png"; done
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
echo "wrote HopDesk.icns, HopDesk.ico, icon-256.png, icon-full-256.png in assets/icon"
