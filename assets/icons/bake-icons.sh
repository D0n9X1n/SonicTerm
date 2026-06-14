#!/usr/bin/env bash
# Bake all icon assets from the source master.
#
# Input (assets/icons/source/):
#   sonic.png        — full-color app icon (squircle)
#
# Outputs (assets/icons/exports/):
#   png/sonic-{16,32,48,64,128,256,512,1024}.png  (full-color)
#   png/sonic-{16,32,64,128,256,512}@2x.png (retina pairs)
#   sonic.icns      — macOS bundle (built via iconutil; mac only)
#   sonic.ico       — Windows multi-resolution (built via ImageMagick)
#
# Dependencies:
#   magick (ImageMagick) or sips for PNG resizing
#   iconutil for .icns (macOS, part of the OS)
#   magick for .ico
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC="$ROOT/assets/icons/source"
OUT="$ROOT/assets/icons/exports"
PNG="$OUT/png"
rm -rf "$OUT"
mkdir -p "$PNG"

render_png() {
    local png="$1" size="$2" out="$3"
    if command -v magick >/dev/null 2>&1; then
        magick "$png" -resize "${size}x${size}" "$out"
    elif command -v sips >/dev/null 2>&1; then
        sips -z "$size" "$size" "$png" --out "$out" >/dev/null
    else
        echo "Need magick or sips for PNG resizing" >&2
        exit 1
    fi
}

echo "==> Full-color PNGs"
for s in 16 32 48 64 128 256 512 1024; do
    render_png "$SRC/sonic.png" "$s" "$PNG/sonic-${s}.png"
done
echo "==> Retina @2x PNGs"
for s in 16 32 64 128 256 512; do
    render_png "$SRC/sonic.png" "$((s * 2))" "$PNG/sonic-${s}@2x.png"
done

# ---- macOS .icns ----
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo "==> Building sonic.icns"
    ICONSET="$OUT/sonic.iconset"
    rm -rf "$ICONSET"; mkdir -p "$ICONSET"
    cp "$PNG/sonic-16.png"     "$ICONSET/icon_16x16.png"
    cp "$PNG/sonic-32.png"     "$ICONSET/icon_16x16@2x.png"
    cp "$PNG/sonic-32.png"     "$ICONSET/icon_32x32.png"
    cp "$PNG/sonic-64.png"     "$ICONSET/icon_32x32@2x.png"
    cp "$PNG/sonic-128.png"    "$ICONSET/icon_128x128.png"
    cp "$PNG/sonic-256.png"    "$ICONSET/icon_128x128@2x.png"
    cp "$PNG/sonic-256.png"    "$ICONSET/icon_256x256.png"
    cp "$PNG/sonic-512.png"    "$ICONSET/icon_256x256@2x.png"
    cp "$PNG/sonic-512.png"    "$ICONSET/icon_512x512.png"
    cp "$PNG/sonic-1024.png"   "$ICONSET/icon_512x512@2x.png"
    iconutil -c icns "$ICONSET" -o "$OUT/sonic.icns"
    rm -rf "$ICONSET"
fi

# ---- Windows .ico ----
if command -v magick >/dev/null 2>&1; then
    echo "==> Building sonic.ico"
    magick \
        "$PNG/sonic-16.png" "$PNG/sonic-32.png" "$PNG/sonic-48.png" \
        "$PNG/sonic-64.png" "$PNG/sonic-128.png" "$PNG/sonic-256.png" \
        "$OUT/sonic.ico"
    cp "$OUT/sonic.ico" "$OUT/sonic-windows-taskbar.ico"
fi

echo
echo "==> Done. Artifacts:"
ls -lh "$OUT"
ls -lh "$PNG"
