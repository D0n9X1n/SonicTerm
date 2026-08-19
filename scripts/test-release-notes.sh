#!/usr/bin/env bash
# Unit test for scripts/release-notes.sh. Uses a temporary git repository.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/sonic-release-notes.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

(
  cd "$TMP"
  git init -q
  git config user.email test@example.invalid
  git config user.name "Release Notes Test"
  echo one > file.txt
  git add file.txt
  git commit -q -m "feat: initial release"
  git tag v0.9.2
  echo two >> file.txt
  git commit -am "fix: polish palette" -q
  git tag v0.9.1
  cat > release-assets.json <<'JSON'
{
  "schema_version": 1,
  "tag": "v0.9.1",
  "assets": [
    {"name":"SonicTerm-v0.9.1-linux-x86_64.deb","path":"SonicTerm-v0.9.1-linux-x86_64.deb","platform":"linux","arch":"x86_64","kind":"deb","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
    {"name":"SonicTerm-v0.9.1-linux-x86_64.tar.gz","path":"SonicTerm-v0.9.1-linux-x86_64.tar.gz","platform":"linux","arch":"x86_64","kind":"tar.gz","sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
    {"name":"SonicTerm-v0.9.1-mac-aarch64.dmg","path":"SonicTerm-v0.9.1-mac-aarch64.dmg","platform":"macos","arch":"aarch64","kind":"dmg","sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},
    {"name":"SonicTerm-v0.9.1-mac-x86_64.dmg","path":"SonicTerm-v0.9.1-mac-x86_64.dmg","platform":"macos","arch":"x86_64","kind":"dmg","sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"},
    {"name":"SonicTerm-v0.9.1-windows-x86_64.msi","path":"SonicTerm-v0.9.1-windows-x86_64.msi","platform":"windows","arch":"x86_64","kind":"msi","sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}
  ]
}
JSON
  "$ROOT/scripts/release-notes.sh" v0.9.1 release-assets.json > notes.md
  grep -q "SonicTerm v0.9.1" notes.md
  grep -q "Changes since v0.9.2" notes.md
  grep -q "fix: polish palette" notes.md
  grep -q "SonicTerm-v0.9.1-mac-aarch64.dmg" notes.md
  grep -q "SonicTerm-v0.9.1-mac-x86_64.dmg" notes.md
  grep -q "SonicTerm-v0.9.1-windows-x86_64.msi" notes.md
  grep -q "SonicTerm-v0.9.1-linux-x86_64.tar.gz" notes.md
  grep -q "SonicTerm-v0.9.1-linux-x86_64.deb" notes.md
  grep -q "release-assets.json" notes.md
  grep -q "SHA256SUMS.txt" notes.md
)

echo "release-notes.sh test passed"
