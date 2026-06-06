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
  git tag v1.0.0
  "$ROOT/scripts/release-notes.sh" v1.0.0 > notes.md
  grep -q "SonicTerm v1.0.0" notes.md
  grep -q "Changes since v0.9.2" notes.md
  grep -q "fix: polish palette" notes.md
  grep -q "SonicTerm-v1.0.0-mac-universal.dmg" notes.md
)

echo "release-notes.sh test passed"
