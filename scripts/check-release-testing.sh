#!/usr/bin/env bash
# Release-gate: ensure docs/RELEASE_TESTING.md is fully checked off before tagging.
#
# Usage:
#   bash scripts/check-release-testing.sh
#
# Exits 0 only when every `[ ]` in docs/RELEASE_TESTING.md has been ticked
# to `[x]` (or `[X]`). Exits 1 listing the unchecked items otherwise.
#
# Also enforced in CI on tag-push via .github/workflows/release.yml.

set -euo pipefail

DOC="docs/RELEASE_TESTING.md"

if [[ ! -f "$DOC" ]]; then
  echo "ERROR: $DOC not found (run from repo root)." >&2
  exit 1
fi

# Find every unchecked checkbox line: leading whitespace, "- [ ]", space, text.
# `grep -n` so we can report line numbers; `|| true` because grep exits 1 on no-match.
unchecked=$(grep -nE '^[[:space:]]*-[[:space:]]+\[[[:space:]]\][[:space:]]' "$DOC" || true)

if [[ -z "$unchecked" ]]; then
  total=$(grep -cE '^[[:space:]]*-[[:space:]]+\[[xX]\][[:space:]]' "$DOC" || echo 0)
  echo "OK: all $total checklist items in $DOC are checked."
  exit 0
fi

count=$(printf '%s\n' "$unchecked" | wc -l | tr -d ' ')
echo "FAIL: $count unchecked item(s) in $DOC:" >&2
printf '%s\n' "$unchecked" >&2
echo "" >&2
echo "Run the checklist locally on a freshly built release binary," >&2
echo "tick every box, commit, and re-run this script before tagging." >&2
exit 1
