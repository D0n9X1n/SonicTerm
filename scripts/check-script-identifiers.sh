#!/usr/bin/env bash
# Run the checker's contract tests before enforcing the naming rule so a parser
# regression cannot turn an empty or misread scan into a green gate.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  printf 'python3 or python is required for the script identifier checker\n' >&2
  exit 1
fi

(
  cd "$ROOT/scripts"
  "$PY" check-script-identifiers_tests.py
)

exec "$PY" "$ROOT/scripts/check-script-identifiers.py" --root "$ROOT"
