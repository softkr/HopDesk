#!/bin/sh
# Fetches the Lucide icons the launcher draws (assets/icons/lucide, ISC licensed) at a pinned version.
# The SVGs stay as published (24 px grid, 2 px stroke) except that the stroke is white, so egui can
# tint them to any palette color. To add an icon: append its name (see lucide.dev) to ICONS, run
# this script, and add a variant to `widgets::Icon`.
set -eu

VERSION=1.74.0
ICONS="check circle-alert circle-check copy ellipsis-vertical info key-round laptop monitor
plug plus route search server shield-check square-terminal trash-2
chevron-right power x refresh-cw activity sliders-horizontal
arrow-up hand lock mouse-pointer-2 sparkles square zap
globe cpu user screen-share arrow-right file-key folder"
BASE="https://unpkg.com/lucide-static@$VERSION"

cd "$(dirname "$0")/../assets/icons/lucide"
for name in $ICONS; do
    curl -sfL "$BASE/icons/$name.svg" | sed 's/currentColor/#fff/' > "$name.svg"
done
curl -sfL "$BASE/LICENSE" > LICENSE
echo "Fetched $(echo $ICONS | wc -w | tr -d ' ') icons from lucide-static $VERSION"
