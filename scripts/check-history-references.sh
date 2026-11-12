#!/usr/bin/env bash
# Run the checker's contract tests before enforcing the rule, so a pattern or scope regression cannot turn a
# misread scan into a green gate.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  printf 'python3 or python is required for the history reference checker\n' >&2
  exit 1
fi

(
  cd "$ROOT/scripts"
  "$PY" check-history-references_tests.py
)

exec "$PY" "$ROOT/scripts/check-history-references.py" --root "$ROOT"
