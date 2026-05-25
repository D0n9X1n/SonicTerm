#!/usr/bin/env bash
# Bake assets/icons/sonic.svg into platform icon bundles.
# Requires: rsvg-convert (cairo) or `inkscape`, and `iconutil` (mac) / ImageMagick (win).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SVG="$SCRIPT_DIR/sonic.svg"
OUT="$SCRIPT_DIR/out"
mkdir -p "$OUT"

render() {
    local size="$1" path="$2"
    if command -v rsvg-convert >/dev/null 2>&1; then
        rsvg-convert -w "$size" -h "$size" "$SVG" -o "$path"
    elif command -v inkscape >/dev/null 2>&1; then
        inkscape -w "$size" -h "$size" "$SVG" -o "$path" >/dev/null 2>&1
    elif command -v magick >/dev/null 2>&1; then
        magick -background none -density 1200 -resize "${size}x${size}" "$SVG" "$path"
    else
        echo "Need rsvg-convert, inkscape, or magick" >&2; exit 1
    fi
}

echo "==> Rendering PNGs..."
for s in 16 32 48 64 128 256 512 1024; do
    render "$s" "$OUT/sonic-${s}.png"
done

# macOS .icns
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo "==> Building sonic.icns"
    ICONSET="$OUT/sonic.iconset"
    rm -rf "$ICONSET"; mkdir -p "$ICONSET"
    cp "$OUT/sonic-16.png"   "$ICONSET/icon_16x16.png"
    cp "$OUT/sonic-32.png"   "$ICONSET/icon_16x16@2x.png"
    cp "$OUT/sonic-32.png"   "$ICONSET/icon_32x32.png"
    cp "$OUT/sonic-64.png"   "$ICONSET/icon_32x32@2x.png"
    cp "$OUT/sonic-128.png"  "$ICONSET/icon_128x128.png"
    cp "$OUT/sonic-256.png"  "$ICONSET/icon_128x128@2x.png"
    cp "$OUT/sonic-256.png"  "$ICONSET/icon_256x256.png"
    cp "$OUT/sonic-512.png"  "$ICONSET/icon_256x256@2x.png"
    cp "$OUT/sonic-512.png"  "$ICONSET/icon_512x512.png"
    cp "$OUT/sonic-1024.png" "$ICONSET/icon_512x512@2x.png"
    iconutil -c icns "$ICONSET" -o "$OUT/sonic.icns"
fi

# Windows .ico (via ImageMagick if available)
if command -v magick >/dev/null 2>&1; then
    echo "==> Building sonic.ico"
    magick "$OUT/sonic-16.png" "$OUT/sonic-32.png" "$OUT/sonic-48.png" \
           "$OUT/sonic-64.png" "$OUT/sonic-128.png" "$OUT/sonic-256.png" \
           "$OUT/sonic.ico"
fi

echo "==> Done. Artifacts in $OUT/"
ls -lh "$OUT"
