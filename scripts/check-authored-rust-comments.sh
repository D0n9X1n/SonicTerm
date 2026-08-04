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
  printf 'python3 or python is required for the Rust comment checker\n' >&2
  exit 1
fi

(
  cd "$ROOT/scripts"
  "$PY" check-authored-rust-comments_tests.py
)

if [[ "${1:-}" == "--self-test" ]]; then
  [[ $# -eq 1 ]] || {
    printf '%s\n' '--self-test accepts no additional arguments' >&2
    exit 2
  }
  exit 0
fi

exec "$PY" "$ROOT/scripts/check-authored-rust-comments.py" --check "$@"
