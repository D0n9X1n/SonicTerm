#!/usr/bin/env bash
# Build a macOS .app bundle and wrap it in a .dmg.
# Usage: scripts/make-macos-dmg.sh <binary> <version> [artifact-suffix] [--bundle-only]
set -euo pipefail

BIN="${1:?binary path required}"
VERSION="${2:?version required}"
ARTIFACT_SUFFIX="${3:-mac-local}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${SONICTERM_PACKAGE_DIR:-$ROOT/dist}"
APP="$DIST/SonicTerm.app"
MODE="${4:-}"
MAX_MINIMUM="${SONICTERM_MAX_MACOS_MINIMUM:-14.0}"
if [ -n "$MODE" ] && [ "$MODE" != "--bundle-only" ]; then
    printf 'unsupported packaging mode: %s\n' "$MODE" >&2
    exit 2
fi

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
mkdir -p "$APP/Contents/Resources/assets"
cp -R "$ROOT/assets/fonts"   "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/themes"  "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/keymaps" "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/icons"   "$APP/Contents/Resources/assets/"
cp -R "$ROOT/assets/i18n"    "$APP/Contents/Resources/assets/"

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
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key> <string>Shell Script</string>
            <key>CFBundleTypeRole</key> <string>Shell</string>
            <key>LSHandlerRank</key>    <string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.shell-script</string>
                <string>com.apple.terminal.shell-script</string>
            </array>
        </dict>
    </array>
    <key>LSMinimumSystemVersion</key>   <string>14.0</string>
    <key>ATSApplicationFontsPath</key>  <string>assets/fonts</string>
    <key>NSHighResolutionCapable</key>  <true/>
    <key>NSPrincipalClass</key>         <string>NSApplication</string>
</dict>
</plist>
PLIST

for font in Regular Italic Bold BoldItalic; do
    test -f "$APP/Contents/Resources/assets/fonts/RecMonoSt.Helens-${font}.ttf"
done

python3 "$ROOT/scripts/macos-bundle.py" bundle "$APP" --max-minimum-macos "$MAX_MINIMUM"
mkdir -p "$APP/Contents/Resources/licenses"
cp "$ROOT/third_party/winit/LICENSE" "$APP/Contents/Resources/licenses/LICENSE-winit-Apache-2.0"

# Seal the fully-assembled bundle with an ad-hoc signature.
#
# We have no Apple Developer ID, so we can neither Developer-ID-sign nor
# notarize. But copying Resources AFTER the binary's linker signature leaves the
# bundle UNSEALED (no Contents/_CodeSignature/CodeResources). A quarantined,
# unsealed bundle is reported by Gatekeeper as "damaged" — a hard block users
# cannot override. Re-signing the bundle ad-hoc AFTER all resources are in place
# makes it internally consistent, downgrading that to the normal, overridable
# "unidentified developer" prompt (right-click → Open, or strip quarantine).
# See the Packaging wiki page for packaging and trust details.
echo "==> Ad-hoc signing $APP (no Developer ID; not notarized)"
codesign --force --sign - "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
python3 "$ROOT/scripts/macos-bundle.py" verify "$APP" --max-minimum-macos "$MAX_MINIMUM"

if [ "$MODE" = "--bundle-only" ]; then
    exit 0
fi

echo "==> Creating .dmg"
DMG="$DIST/SonicTerm-${VERSION}-${ARTIFACT_SUFFIX}.dmg"
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
