#!/usr/bin/env bash
# Build reproducible Linux tar.gz and Debian packages from one staged payload.
set -euo pipefail

usage() {
  printf 'usage: %s [--stage-only] <binary> <tag> [dist-dir]\n' "$0" >&2
  exit 2
}

stage_only=false
if [[ "${1:-}" == "--stage-only" ]]; then
  stage_only=true
  shift
fi
[[ $# -ge 2 && $# -le 3 ]] || usage

binary="$1"
tag="$2"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="${3:-$root/dist}"
mkdir -p "$dist"
dist="$(cd "$dist" && pwd -P)"
identity="com.d0n9x1n.SonicTerm"
payload_name="SonicTerm-${tag}-linux-x86_64"
work="$dist/.linux-package-work"
payload="$work/$payload_name"
tarball="$dist/SonicTerm-${tag}-linux-x86_64.tar.gz"
deb="$dist/SonicTerm-${tag}-linux-x86_64.deb"
version="${tag#v}"

[[ -f "$binary" ]] || { printf 'binary not found: %s\n' "$binary" >&2; exit 1; }
[[ "$tag" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+([+~.-][A-Za-z0-9.+~-]+)?$ ]] || {
  printf 'invalid release tag: %s\n' "$tag" >&2
  exit 1
}

if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH="$(git -C "$root" show -s --format=%ct "$tag" 2>/dev/null || \
    git -C "$root" show -s --format=%ct HEAD)"
fi
export SOURCE_DATE_EPOCH

rm -rf "$work"
mkdir -p "$payload/assets" \
  "$payload/share/applications" \
  "$payload/share/metainfo" \
  "$payload/share/icons/hicolor/256x256/apps"
install -m 0755 "$binary" "$payload/sonicterm"
for directory in fonts themes keymaps icons i18n; do
  cp -R "$root/assets/$directory" "$payload/assets/"
done
install -m 0644 \
  "$root/crates/sonicterm-linux/resources/$identity.desktop" \
  "$payload/share/applications/$identity.desktop"
install -m 0644 \
  "$root/crates/sonicterm-linux/resources/$identity.metainfo.xml" \
  "$payload/share/metainfo/$identity.metainfo.xml"
install -m 0644 \
  "$root/assets/icons/exports/png/sonic-256.png" \
  "$payload/share/icons/hicolor/256x256/apps/$identity.png"
install -m 0644 "$root/LICENSE" "$payload/LICENSE"
install -m 0644 \
  "$root/crates/sonicterm-linux/resources/LICENSE-Rec-Mono-OFL-1.1" \
  "$payload/LICENSE-Rec-Mono-OFL-1.1"
install -m 0644 "$root/README.md" "$payload/README.md"
find "$payload" -type d -exec chmod 0755 {} +
find "$payload" -type f ! -path "$payload/sonicterm" -exec chmod 0644 {} +
find "$payload" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} + 2>/dev/null || \
  find "$payload" -exec touch -h -t "$(date -u -r "$SOURCE_DATE_EPOCH" +%Y%m%d%H%M.%S)" {} +

for face in Regular Bold Italic BoldItalic; do
  test -f "$payload/assets/fonts/RecMonoSt.Helens-${face}.ttf"
done
test -f "$payload/assets/keymaps/sonicterm-linux.toml"
test -f "$payload/assets/themes/wezterm.toml"
test -f "$payload/assets/i18n/en/messages.ftl"
test -f "$payload/assets/i18n/zh-CN/messages.ftl"
test -f "$payload/assets/icons/exports/png/sonic-256.png"

if "$stage_only"; then
  printf 'staged Linux payload: %s\n' "$payload"
  exit 0
fi

[[ "$(uname -s)" == "Linux" ]] || {
  printf 'full Linux packaging requires a Linux host; use --stage-only elsewhere\n' >&2
  exit 1
}
[[ "$(uname -m)" == "x86_64" ]] || {
  printf 'Linux release packages require x86_64, got %s\n' "$(uname -m)" >&2
  exit 1
}
for command in tar gzip dpkg-deb dpkg-shlibdeps readelf file perl python3; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'required package tool is missing: %s\n' "$command" >&2
    exit 1
  }
done
file "$payload/sonicterm" | grep -Fq 'ELF 64-bit LSB' || {
  printf 'release binary is not a 64-bit Linux ELF\n' >&2
  exit 1
}

python3 - "$payload/sonicterm" <<'PY'
import re
import subprocess
import sys

binary = sys.argv[1]
text = subprocess.check_output(["readelf", "--version-info", binary], text=True)
versions = [tuple(map(int, match.groups())) for match in re.finditer(r"GLIBC_(\d+)\.(\d+)", text)]
if not versions:
    raise SystemExit("binary exposes no GLIBC version requirements")
if max(versions) > (2, 35):
    raise SystemExit(f"binary requires GLIBC_{max(versions)[0]}.{max(versions)[1]}, newer than GLIBC_2.35")
PY

rm -f "$tarball" "$deb"
tar --sort=name \
  --mtime="@$SOURCE_DATE_EPOCH" \
  --owner=0 --group=0 --numeric-owner \
  -C "$work" -cf - "$payload_name" | gzip -n -9 > "$tarball"

deb_root="$work/deb-root"
mkdir -p \
  "$deb_root/DEBIAN" \
  "$deb_root/usr/bin" \
  "$deb_root/usr/share/sonicterm" \
  "$deb_root/usr/share/applications" \
  "$deb_root/usr/share/metainfo" \
  "$deb_root/usr/share/icons/hicolor/256x256/apps" \
  "$deb_root/usr/share/doc/sonicterm"
install -m 0755 "$payload/sonicterm" "$deb_root/usr/bin/sonicterm"
cp -R "$payload/assets" "$deb_root/usr/share/sonicterm/"
install -m 0644 "$payload/share/applications/$identity.desktop" \
  "$deb_root/usr/share/applications/$identity.desktop"
install -m 0644 "$payload/share/metainfo/$identity.metainfo.xml" \
  "$deb_root/usr/share/metainfo/$identity.metainfo.xml"
install -m 0644 "$payload/share/icons/hicolor/256x256/apps/$identity.png" \
  "$deb_root/usr/share/icons/hicolor/256x256/apps/$identity.png"
install -m 0644 "$payload/LICENSE" "$deb_root/usr/share/doc/sonicterm/copyright"
install -m 0644 "$payload/LICENSE-Rec-Mono-OFL-1.1" \
  "$deb_root/usr/share/doc/sonicterm/LICENSE-Rec-Mono-OFL-1.1"
install -m 0644 "$payload/README.md" "$deb_root/usr/share/doc/sonicterm/README.md"

substvars="$work/substvars"
mkdir -p "$work/debian"
cat > "$work/debian/control" <<CONTROL
Source: sonicterm
Section: utils
Priority: optional
Maintainer: SonicTerm contributors <noreply@users.noreply.github.com>

Package: sonicterm
Architecture: amd64
Description: Fast GPU-accelerated terminal emulator
CONTROL
(
  cd "$work"
  dpkg-shlibdeps -O -e"$deb_root/usr/bin/sonicterm"
) > "$substvars"
depends="$(perl -ne 'print $1 if /^shlibs:Depends=(.*)$/' "$substvars")"
[[ -n "$depends" ]] || {
  printf 'dpkg-shlibdeps produced no shlibs:Depends\n' >&2
  exit 1
}

cat > "$deb_root/DEBIAN/control" <<CONTROL
Package: sonicterm
Version: $version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: SonicTerm contributors <noreply@users.noreply.github.com>
Depends: $depends
Recommends: xdg-desktop-portal, xdg-utils, mesa-vulkan-drivers
Description: Fast GPU-accelerated terminal emulator
 SonicTerm provides split panes, searchable commands, configurable keybindings,
 and bundled typography in a native terminal application.
CONTROL

find "$deb_root" -type d -exec chmod 0755 {} +
find "$deb_root" -type f ! -path "$deb_root/usr/bin/sonicterm" -exec chmod 0644 {} +
find "$deb_root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +

dpkg-deb --root-owner-group --build "$deb_root" "$deb"

printf 'built %s\n' "$tarball"
printf 'built %s\n' "$deb"
