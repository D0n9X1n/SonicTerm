#!/usr/bin/env bash
# Build a macOS .app bundle and wrap it in a .dmg.
# Usage: make-dmg.sh <path-to-universal-binary> <version>
set -euo pipefail

BIN="${1:?binary path required}"
VERSION="${2:?version required}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/SonicTerm.app"

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/"{MacOS,Resources}

cp "$BIN" "$APP/Contents/MacOS/sonicterm-mac"
chmod +x "$APP/Contents/MacOS/sonicterm-mac"

cp "$ROOT/assets/icons/exports/sonic.icns" "$APP/Contents/Resources/" 2>/dev/null || \
    echo "warning: icon not baked, continuing"

# Bundle runtime assets/ tree — required by crates/sonicterm-mac/src/main.rs
# which loads Contents/Resources/assets/{fonts,themes,keymaps,icons,i18n}/ at
# startup. Without these, fresh-installed DMGs panic with 'Error: load theme'.
# Fixes #451.
mkdir -p "$APP/Contents/Resources/assets"
cp -R "$ROOT/assets/fonts"   "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/themes"  "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/keymaps" "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/icons"   "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/i18n"    "$APP/Contents/Resources/assets/"

# Also expose bundled app fonts via the standard macOS app-font path.
# SonicTerm loads assets/fonts itself, but ATSApplicationFontsPath lets
# CoreText/AppKit resolve Rec Mono St.Helens from the app bundle too.
mkdir -p "$APP/Contents/Resources/Fonts"
cp "$ROOT/assets/fonts"/*.ttf "$APP/Contents/Resources/Fonts/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>             <string>SonicTerm</string>
    <key>CFBundleDisplayName</key>      <string>SonicTerm</string>
    <key>CFBundleIdentifier</key>       <string>com.d0n9x1n.sonicterm</string>
    <key>CFBundleVersion</key>          <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleExecutable</key>       <string>sonicterm-mac</string>
    <key>CFBundleIconFile</key>         <string>sonic</string>
    <key>CFBundlePackageType</key>      <string>APPL</string>
    <key>LSMinimumSystemVersion</key>   <string>14.0</string>
    <key>ATSApplicationFontsPath</key>  <string>Fonts</string>
    <key>NSHighResolutionCapable</key>  <true/>
    <key>NSPrincipalClass</key>         <string>NSApplication</string>
</dict>
</plist>
PLIST

for font in Regular Italic Bold BoldItalic; do
    test -f "$APP/Contents/Resources/assets/fonts/RecMonoSt.Helens-${font}.ttf"
    test -f "$APP/Contents/Resources/Fonts/RecMonoSt.Helens-${font}.ttf"
done

echo "Note: building UNSIGNED .dmg — see CLAUDE.md §9"

echo "==> Creating .dmg"
DMG="$DIST/SonicTerm-${VERSION}-mac-universal.dmg"
rm -f "$DMG"
create-dmg \
    --volname "SonicTerm ${VERSION}" \
    --window-size 600 400 \
    --icon-size 110 \
    --app-drop-link 450 200 \
    --icon "SonicTerm.app" 150 200 \
    "$DMG" \
    "$APP" || {
        # Fallback: hdiutil if create-dmg is missing
        echo "create-dmg failed, falling back to hdiutil"
        hdiutil create -volname "SonicTerm ${VERSION}" -srcfolder "$APP" -ov -format UDZO "$DMG"
    }

echo "==> Built $DMG"
ls -lh "$DMG"
