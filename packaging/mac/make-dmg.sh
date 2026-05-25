#!/usr/bin/env bash
# Build a macOS .app bundle and wrap it in a .dmg.
# Usage: make-dmg.sh <path-to-universal-binary> <version>
set -euo pipefail

BIN="${1:?binary path required}"
VERSION="${2:?version required}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Sonic.app"

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/"{MacOS,Resources}

cp "$BIN" "$APP/Contents/MacOS/sonic"
chmod +x "$APP/Contents/MacOS/sonic"

cp "$ROOT/assets/icons/out/sonic.icns" "$APP/Contents/Resources/" 2>/dev/null || \
    echo "warning: icon not baked, continuing"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>             <string>Sonic</string>
    <key>CFBundleDisplayName</key>      <string>Sonic Terminal</string>
    <key>CFBundleIdentifier</key>       <string>com.sonic.terminal</string>
    <key>CFBundleVersion</key>          <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleExecutable</key>       <string>sonic</string>
    <key>CFBundleIconFile</key>         <string>sonic</string>
    <key>CFBundlePackageType</key>      <string>APPL</string>
    <key>LSMinimumSystemVersion</key>   <string>14.0</string>
    <key>NSHighResolutionCapable</key>  <true/>
    <key>NSPrincipalClass</key>         <string>NSApplication</string>
</dict>
</plist>
PLIST

echo "==> Creating .dmg"
DMG="$DIST/Sonic-${VERSION}-mac-universal.dmg"
rm -f "$DMG"
create-dmg \
    --volname "Sonic ${VERSION}" \
    --window-size 600 400 \
    --icon-size 110 \
    --app-drop-link 450 200 \
    --icon "Sonic.app" 150 200 \
    "$DMG" \
    "$APP" || {
        # Fallback: hdiutil if create-dmg is missing
        echo "create-dmg failed, falling back to hdiutil"
        hdiutil create -volname "Sonic ${VERSION}" -srcfolder "$APP" -ov -format UDZO "$DMG"
    }

echo "==> Built $DMG"
ls -lh "$DMG"
