#!/usr/bin/env bash
# Deterministic contract gate for the real resource baseline evidence collector.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COLLECTOR="$ROOT/scripts/resource-baseline-evidence.py"
TESTS="$ROOT/scripts/resource-baseline-evidence_tests.py"

if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  echo "python3 or python is required to test resource baseline evidence" >&2
  exit 1
fi

"$PY" -m py_compile "$COLLECTOR" "$TESTS"
"$PY" "$TESTS"
"$PY" "$COLLECTOR" --help >/dev/null

echo "resource baseline evidence tests passed"
