#!/bin/sh
# Fetches the Lucide icons the launcher draws (assets/icons/lucide, ISC licensed) at a pinned version.
# The SVGs stay as published (24 px grid, 2 px stroke) except that the stroke is white, so egui can
# tint them to any palette color. To add an icon: append its name (see lucide.dev) to ICONS, run
# this script, and add a variant to `widgets::Icon`.
set -eu

VERSION=1.74.0
ICONS="check circle-alert circle-check copy ellipsis-vertical folder-open info key-round laptop monitor
network pencil plug plus ratio route search server shield-check square-terminal trash-2
user-round chevron-right power list x"
BASE="https://unpkg.com/lucide-static@$VERSION"

cd "$(dirname "$0")/../assets/icons/lucide"
for name in $ICONS; do
    curl -sfL "$BASE/icons/$name.svg" | sed 's/currentColor/#fff/' > "$name.svg"
done
curl -sfL "$BASE/LICENSE" > LICENSE
echo "Fetched $(echo $ICONS | wc -w | tr -d ' ') icons from lucide-static $VERSION"
