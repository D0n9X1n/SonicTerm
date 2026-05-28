#!/usr/bin/env bash
# Release-gate: ensure docs/RELEASE_TESTING.md is fully checked off before tagging.
#
# Usage:
#   bash scripts/check-release-testing.sh
#
# Exhausts the WHOLE document on every run so the maintainer sees the
# complete gap in one shot — not just the first failure. Reports:
#   * Every `- [ ]` unchecked checklist item (with line numbers).
#   * Every `needs-test` marker in a §37 table row (with line numbers).
#     (The TODO list at the bottom intentionally tracks the same gaps as
#      regular `- [ ]` boxes; those are counted under "unchecked items".)
#
# Exit 0 only when BOTH counts are zero. Exit 1 otherwise.
#
# Also enforced in CI on tag-push via .github/workflows/release.yml.

set -uo pipefail

DOC="docs/RELEASE_TESTING.md"

if [[ ! -f "$DOC" ]]; then
  echo "ERROR: $DOC not found (run from repo root)." >&2
  exit 1
fi

# --- Scan 1: unchecked checklist boxes --------------------------------------
# Match leading whitespace, "- [ ]", required space, text.
unchecked=$(grep -nE '^[[:space:]]*-[[:space:]]+\[[[:space:]]\][[:space:]]' "$DOC" || true)
if [[ -n "$unchecked" ]]; then
  unchecked_count=$(printf '%s\n' "$unchecked" | wc -l | tr -d ' ')
else
  unchecked_count=0
fi

# --- Scan 2: `needs-test` markers in §37 table rows -------------------------
# Table rows contain `|` separators. The TODO bullets do not — they're
# `- [ ]` checklist items already counted by Scan 1.
needs_test=$(grep -nE 'needs-test' "$DOC" | grep -E '\|' || true)
if [[ -n "$needs_test" ]]; then
  needs_test_count=$(printf '%s\n' "$needs_test" | wc -l | tr -d ' ')
else
  needs_test_count=0
fi

# --- Report -----------------------------------------------------------------
checked_total=$(grep -cE '^[[:space:]]*-[[:space:]]+\[[xX]\][[:space:]]' "$DOC" || true)

echo "Release-testing gap report for $DOC"
echo "-------------------------------------------------------------"
echo "Unchecked items: $unchecked_count   (checked: $checked_total)"
if [[ $unchecked_count -gt 0 ]]; then
  printf '%s\n' "$unchecked" | sed 's/^/  /'
fi
echo ""
echo "needs-test gaps (§37 table): $needs_test_count"
if [[ $needs_test_count -gt 0 ]]; then
  printf '%s\n' "$needs_test" | sed 's/^/  /'
fi
echo "-------------------------------------------------------------"

if [[ $unchecked_count -eq 0 && $needs_test_count -eq 0 ]]; then
  echo "OK: all $checked_total checklist items checked and no needs-test gaps."
  exit 0
fi

echo "FAIL: tick remaining boxes and close needs-test gaps before tagging." >&2
exit 1
