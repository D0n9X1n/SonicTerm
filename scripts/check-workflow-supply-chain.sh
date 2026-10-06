#!/usr/bin/env bash
# Run the checker contract tests before enforcing the repository policy so a
# parser regression cannot turn an empty or misclassified scan into a green gate.
# The local-gate runner and parity tests run here too, so every CI job that runs
# this script also checks the gate table against ci.yml, CLAUDE.md, and both
# wiki files. The tests of the scripts that import local-gate.py's launcher, the
# native selection smoke and the performance comparison, also run here, so they
# run on macOS, Windows, and Linux, as do the critical-path accounting's tests.

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
  "$ROOT/scripts/check-workflow-supply-chain_tests.py" \
  "$ROOT/scripts/local-gate.py" \
  "$ROOT/scripts/local-gate_tests.py" \
  "$ROOT/scripts/windows-process-job.py" \
  "$ROOT/scripts/windows-process-bootstrap.py" \
  "$ROOT/scripts/native-selection-smoke.py" \
  "$ROOT/scripts/native-selection-smoke_tests.py" \
  "$ROOT/scripts/perf-compare.py" \
  "$ROOT/scripts/perf-compare_tests.py" \
  "$ROOT/scripts/perf-critical-path.py" \
  "$ROOT/scripts/perf-critical-path_tests.py" \
  "$ROOT/scripts/perf-flags.py" \
  "$ROOT/scripts/perf-flags_tests.py"

(
  cd "$ROOT/scripts"
  "$PY" check-workflow-supply-chain_tests.py
  "$PY" local-gate_tests.py
  "$PY" native-selection-smoke_tests.py
  "$PY" perf-compare_tests.py
  "$PY" perf-critical-path_tests.py
  "$PY" perf-flags_tests.py
)

exec "$PY" "$ROOT/scripts/check-workflow-supply-chain.py" --root "$ROOT"
