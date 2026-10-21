#!/bin/sh
# Regenerates the PWA icons from assets/brand/taba-app-icon.svg. Needs rsvg-convert (librsvg).
# "any" icons keep the rounded corners; the maskable icon and the iOS touch icon are full bleed
# (the OS applies its own mask), and the mark already sits inside the maskable safe zone.
set -eu
cd "$(dirname "$0")/.."
src=../assets/brand/taba-app-icon.svg
out=public/icons
full=$(mktemp)
sed 's/ rx="27"//' "$src" > "$full"
rsvg-convert -w 192 -h 192 "$src" -o "$out/icon-192.png"
rsvg-convert -w 512 -h 512 "$src" -o "$out/icon-512.png"
rsvg-convert -w 512 -h 512 "$full" -o "$out/icon-maskable-512.png"
rsvg-convert -w 180 -h 180 "$full" -o "$out/apple-touch-icon.png"
cp "$src" public/favicon.svg
rm "$full"
