#!/usr/bin/env bash
# Run the checker contract tests before enforcing the repository policy so a
# parser regression cannot turn an empty or misclassified scan into a green gate.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  printf 'python3 or python is required for the workflow supply-chain checker\n' >&2
  exit 1
fi

"$PY" -m py_compile \
  "$ROOT/scripts/check-workflow-supply-chain.py" \
  "$ROOT/scripts/check-workflow-supply-chain_tests.py"

(
  cd "$ROOT/scripts"
  "$PY" check-workflow-supply-chain_tests.py
)

exec "$PY" "$ROOT/scripts/check-workflow-supply-chain.py" --root "$ROOT"
