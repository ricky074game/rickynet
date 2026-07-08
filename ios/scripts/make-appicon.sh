#!/usr/bin/env bash
#
# Rasterize assets/icon.svg into ios/Resources/Assets.xcassets/AppIcon.appiconset.
# Renders each required size with rsvg-convert (crisp, exact pixels) and flattens
# onto an opaque background so the PNGs carry NO alpha channel (asset catalogs /
# the App Store reject alpha in the marketing icon).
#
# Requires: rsvg-convert (brew install librsvg) and ImageMagick (brew install
# imagemagick). Usage: ios/scripts/make-appicon.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SVG="$ROOT/assets/icon.svg"
OUT="$ROOT/ios/Resources/Assets.xcassets/AppIcon.appiconset"
# Opaque fill behind the (already full-bleed) icon; matches the gradient's
# top-left so any edge antialiasing blends invisibly.
BG="#2563EB"

command -v rsvg-convert >/dev/null 2>&1 || { echo "ERROR: rsvg-convert not found (brew install librsvg)"; exit 1; }
if command -v magick >/dev/null 2>&1; then IM="magick"; elif command -v convert >/dev/null 2>&1; then IM="convert"; else
  echo "ERROR: ImageMagick not found (brew install imagemagick)"; exit 1
fi

[ -f "$SVG" ] || { echo "ERROR: $SVG not found"; exit 1; }
mkdir -p "$OUT"

render() { # <filename> <pixels>
  local name="$1" px="$2"
  rsvg-convert -w "$px" -h "$px" "$SVG" -o "$OUT/$name"
  "$IM" "$OUT/$name" -background "$BG" -alpha remove -alpha off "$OUT/$name"
}

# filename                pixels  (idiom/size/scale — see Contents.json)
render Icon-20@2x.png       40   # iphone 20@2x
render Icon-20@3x.png       60   # iphone 20@3x
render Icon-29@2x.png       58   # iphone 29@2x
render Icon-29@3x.png       87   # iphone 29@3x
render Icon-40@2x.png       80   # iphone 40@2x
render Icon-40@3x.png      120   # iphone 40@3x
render Icon-60@2x.png      120   # iphone 60@2x  (app @2x)
render Icon-60@3x.png      180   # iphone 60@3x  (app @3x)
render Icon-20.png          20   # ipad 20@1x
render Icon-20@2x-ipad.png  40   # ipad 20@2x
render Icon-29.png          29   # ipad 29@1x
render Icon-29@2x-ipad.png  58   # ipad 29@2x
render Icon-40.png          40   # ipad 40@1x
render Icon-40@2x-ipad.png  80   # ipad 40@2x
render Icon-76.png          76   # ipad 76@1x
render Icon-76@2x.png      152   # ipad 76@2x  (app)
render Icon-83.5@2x.png    167   # ipad pro 83.5@2x
render Icon-1024.png      1024   # ios-marketing

echo "Generated app icons in $OUT:"
ls -1 "$OUT"/*.png | while read -r f; do
  printf '  %-22s %s\n' "$(basename "$f")" "$($IM identify -format '%wx%h alpha=%A' "$f" 2>/dev/null || echo '?')"
done
echo "Done."
