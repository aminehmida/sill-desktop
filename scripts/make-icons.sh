#!/usr/bin/env bash
# Renders the app's icons from design/AppIcon.svg (and AppIcon-small.svg for 48 pixels and under)
# into the files the build and the packages use. Run on a Mac after changing either drawing; the
# results are committed.   Needs rsvg-convert (brew install librsvg) and Python with Pillow.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
render() { # size → the small drawing up to 48 pixels, the full one above
  if [ "$1" -le 48 ]; then rsvg-convert -w "$1" -h "$1" design/AppIcon-small.svg -o "$2"
  else rsvg-convert -w "$1" -h "$1" design/AppIcon.svg -o "$2"; fi
}
set="$tmp/AppIcon.iconset"; mkdir "$set"
for s in 16 32 128 256 512; do
  render "$s" "$set/icon_${s}x${s}.png"
  render $((s * 2)) "$set/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$set" -o packaging/macos/AppIcon.icns
for s in 16 20 24 32 40 48 64 256; do render "$s" "$tmp/$s.png"; done
python3 - "$tmp" <<'PY'
import sys
from PIL import Image
t = sys.argv[1]
sizes = [16, 20, 24, 32, 40, 48, 64, 256]
imgs = [Image.open(f"{t}/{s}.png").convert("RGBA") for s in sizes]
imgs[-1].save("packaging/windows/icon.ico", format="ICO", append_images=imgs[:-1], sizes=[(s, s) for s in sizes])
PY
render 256 crates/sill-app/assets/icon.png   # the window's own icon (Windows and Linux)
ls -la packaging/macos/AppIcon.icns packaging/windows/icon.ico crates/sill-app/assets/icon.png
