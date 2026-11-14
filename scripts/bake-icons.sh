#!/usr/bin/env bash
# Bake every icon asset from the vector master.
#
# Input:
#   assets/icons/source/sonic.svg            the master; edit only this
#
# Outputs (assets/icons/exports/, committed; packaging reads them as they are):
#   sonic.svg                                the master, copied for Linux `hicolor/scalable`
#   png/sonic-<N>.png                        16 22 24 32 48 64 128 256 512 1024 px
#   png/sonic-<N>@2x.png                     16 32 64 128 256 512 pt at twice the pixels
#   sonic.icns                               macOS bundle icon (iconutil; macOS only)
#   sonic.ico                                Windows exe, taskbar and MSI icon, 16 to 256 px
#
# Each PNG is rendered from the vector at its own size instead of shrunk from one
# large bitmap, so the 16 to 48 px icons that title bars, taskbars and docks show
# keep sharp chevron edges.
#
# Dependencies: rsvg-convert (librsvg) renders, magick (ImageMagick) packs the .ico,
# iconutil packs the .icns. `brew install librsvg imagemagick` on macOS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MASTER="$ROOT/assets/icons/source/sonic.svg"
OUT="$ROOT/assets/icons/exports"
PNG="$OUT/png"

# The Linux hicolor sizes, plus 1024 px for full-size previews.
PNG_SIZES_PX=(16 22 24 32 48 64 128 256 512 1024)
RETINA_SIZES_PT=(16 32 64 128 256 512)
# Windows' title-bar, taskbar and Start sizes at every scale from 100% to 400%. It looks for an
# exact match first, else shrinks the next larger entry, so each missing step is drawn soft.
ICO_SIZES_PX=(16 20 24 30 32 36 40 48 60 64 72 80 96 256)

for tool in rsvg-convert magick; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'bake-icons: %s is required (brew install librsvg imagemagick)\n' "$tool" >&2
        exit 1
    }
done
[[ -f "$MASTER" ]] || { printf 'bake-icons: master %s is missing\n' "$MASTER" >&2; exit 1; }

render_png() {
    local size_px="$1" out="$2"
    rsvg-convert --width "$size_px" --height "$size_px" --keep-aspect-ratio "$MASTER" -o "$out"
}

# The PNG set is rebuilt whole so a dropped size leaves no stale file; the .icns is kept
# until it is rebuilt, so a host without iconutil does not delete the shipped one.
rm -rf "$PNG"
mkdir -p "$PNG"
cp "$MASTER" "$OUT/sonic.svg"

echo "==> PNGs"
for size_px in "${PNG_SIZES_PX[@]}"; do
    render_png "$size_px" "$PNG/sonic-${size_px}.png"
done
for size_pt in "${RETINA_SIZES_PT[@]}"; do
    render_png "$((size_pt * 2))" "$PNG/sonic-${size_pt}@2x.png"
done

# When: iconutil exists only on macOS; elsewhere the committed .icns stays the shipped one.
if command -v iconutil >/dev/null 2>&1; then
    echo "==> sonic.icns"
    ICONSET="$(mktemp -d)/sonic.iconset"
    mkdir -p "$ICONSET"
    for size_pt in 16 32 128 256 512; do
        cp "$PNG/sonic-${size_pt}.png" "$ICONSET/icon_${size_pt}x${size_pt}.png"
        cp "$PNG/sonic-${size_pt}@2x.png" "$ICONSET/icon_${size_pt}x${size_pt}@2x.png"
    done
    iconutil -c icns "$ICONSET" -o "$OUT/sonic.icns"
    rm -rf "$(dirname "$ICONSET")"
else
    printf 'bake-icons: iconutil not found; sonic.icns not rebuilt\n' >&2
fi

echo "==> sonic.ico"
ICO_WORK="$(mktemp -d)"
ico_inputs=()
for size_px in "${ICO_SIZES_PX[@]}"; do
    render_png "$size_px" "$ICO_WORK/sonic-${size_px}.png"
    ico_inputs+=("$ICO_WORK/sonic-${size_px}.png")
done
# Date chunks would change the bytes on every run although the pixels do not.
magick "${ico_inputs[@]}" -define png:exclude-chunks=date,time "$OUT/sonic.ico"
rm -rf "$ICO_WORK"

echo
echo "==> Done"
ls -lh "$OUT" "$PNG"
