#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
builder="$root/scripts/make-linux-packages.sh"

fail() {
  printf 'linux package test: %s\n' "$1" >&2
  exit 1
}

[[ -x "$builder" ]] || fail "package builder is missing or not executable"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/sonicterm-linux-package.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

fake_binary="$tmp/sonicterm"
printf '#!/bin/sh\nexit 0\n' > "$fake_binary"
chmod 755 "$fake_binary"

SOURCE_DATE_EPOCH=1700000000 "$builder" --stage-only "$fake_binary" v9.8.7 "$tmp/dist"
stage="$tmp/dist/.linux-package-work/SonicTerm-v9.8.7-linux-x86_64"

[[ -x "$stage/sonicterm" ]] || fail "portable binary is missing or not executable"
for required in \
  assets/fonts/RecMonoSt.Helens-Regular.ttf \
  assets/fonts/RecMonoSt.Helens-Bold.ttf \
  assets/fonts/RecMonoSt.Helens-Italic.ttf \
  assets/fonts/RecMonoSt.Helens-BoldItalic.ttf \
  assets/keymaps/sonicterm-linux.toml \
  assets/themes/wezterm.toml \
  assets/i18n/en/messages.ftl \
  assets/i18n/zh-CN/messages.ftl \
  assets/icons/exports/png/sonic-256.png \
  share/applications/com.d0n9x1n.SonicTerm.desktop \
  share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml \
  share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png \
  LICENSE \
  LICENSE-Rec-Mono-OFL-1.1 \
  README.md; do
  [[ -f "$stage/$required" ]] || fail "staged payload is missing $required"
done

[[ "$(stat -c '%a' "$stage/sonicterm" 2>/dev/null || stat -f '%Lp' "$stage/sonicterm")" == "755" ]] || \
  fail "staged binary mode is not 755"
[[ "$(stat -c '%a' "$stage/LICENSE" 2>/dev/null || stat -f '%Lp' "$stage/LICENSE")" == "644" ]] || \
  fail "staged data mode is not 644"

for required_text in \
  "SonicTerm-\${tag}-linux-x86_64.tar.gz" \
  "SonicTerm-\${tag}-linux-x86_64.deb" \
  'dpkg-shlibdeps' \
  'debian/control' \
  'SOURCE_DATE_EPOCH' \
  '--sort=name' \
  '--numeric-owner' \
  'dpkg-deb --root-owner-group' \
  'GLIBC_2.35'; do
  grep -Fq -- "$required_text" "$builder" || fail "builder is missing contract text: $required_text"
done

if [[ $# -eq 0 ]]; then
  printf 'linux package test: source contract ok\n'
  exit 0
fi
[[ $# -eq 2 ]] || fail "usage: $0 [<tar.gz> <deb>]"
tarball="$1"
deb="$2"
[[ -f "$tarball" ]] || fail "tarball does not exist: $tarball"
[[ -f "$deb" ]] || fail "Debian package does not exist: $deb"
command -v dpkg-deb >/dev/null 2>&1 || fail "dpkg-deb is required for artifact validation"
command -v readelf >/dev/null 2>&1 || fail "readelf is required for artifact validation"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required for reproducibility validation"

python3 - "$tarball" <<'PY'
import pathlib
import sys

header = pathlib.Path(sys.argv[1]).read_bytes()[:10]
if len(header) != 10 or header[:3] != b"\x1f\x8b\x08":
    raise SystemExit("tarball has no valid gzip header")
if int.from_bytes(header[4:8], "little") != 0:
    raise SystemExit("gzip header retains a build-time timestamp")
if header[3] & 0x08:
    raise SystemExit("gzip header retains an input filename")
PY

portable="$tmp/portable"
deb_root="$tmp/deb-root"
mkdir -p "$portable" "$deb_root"
tar -xzf "$tarball" -C "$portable"
dpkg-deb -x "$deb" "$deb_root"
portable_root="$(find "$portable" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ -n "$portable_root" ]] || fail "tarball has no top-level payload directory"
[[ -x "$portable_root/sonicterm" ]] || fail "tarball binary is missing"
[[ -d "$portable_root/assets" ]] || fail "tarball adjacent assets are missing"
[[ -x "$deb_root/usr/bin/sonicterm" ]] || fail "Debian binary is missing"
[[ -d "$deb_root/usr/share/sonicterm/assets" ]] || fail "Debian assets are missing"

architecture="$(dpkg-deb -f "$deb" Architecture)"
[[ "$architecture" == "amd64" ]] || fail "Debian architecture is $architecture, expected amd64"
depends="$(dpkg-deb -f "$deb" Depends)"
[[ -n "$depends" ]] || fail "Debian Depends is empty"

readelf -h "$deb_root/usr/bin/sonicterm" | grep -Fq 'Advanced Micro Devices X86-64' || \
  fail "Debian executable is not x86_64 ELF"
if readelf --version-info "$deb_root/usr/bin/sonicterm" | \
  grep -Eo 'GLIBC_[0-9]+\.[0-9]+' | \
  python3 -c 'import sys; versions=[tuple(map(int,line.split("_")[1].split("."))) for line in sys.stdin if line.strip()]; raise SystemExit(0 if versions and max(versions) <= (2,35) else 1)'; then
  :
else
  fail "binary requires glibc newer than 2.35 or exposes no glibc requirement"
fi

if grep -R -Fq -- "$root" "$portable_root" "$deb_root"; then
  fail "package payload contains the build-host repository path"
fi

repro_a="$tmp/repro-a"
repro_b="$tmp/repro-b"
for output in "$repro_a" "$repro_b"; do
  SOURCE_DATE_EPOCH=1700000000 "$builder" "$portable_root/sonicterm" v9.8.7 "$output" >/dev/null
 done
for suffix in tar.gz deb; do
  first="$repro_a/SonicTerm-v9.8.7-linux-x86_64.$suffix"
  second="$repro_b/SonicTerm-v9.8.7-linux-x86_64.$suffix"
  first_line="$(sha256sum "$first")"
  second_line="$(sha256sum "$second")"
  first_digest="${first_line%% *}"
  second_digest="${second_line%% *}"
  [[ "$first_digest" == "$second_digest" ]] || \
    fail "$suffix output is not reproducible under one SOURCE_DATE_EPOCH"
done

printf 'linux package test: artifacts and reproducibility ok\n'
