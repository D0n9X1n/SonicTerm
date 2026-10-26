#!/usr/bin/env bash
set -euo pipefail

# Coverage floor for the macOS-only logic inside the 80% subset. Logic coverage
# runs on Linux, where the AppKit word boundary in sonicterm-ui's text_edit.rs
# and the macOS arms of command_label.rs compile out, so the Linux crate floors
# cannot see them. This script instruments sonicterm-ui alone on macOS and holds
# each file in the baseline's `macos` section to its floor through
# `coverage-floor.py macos-floor`, which enforces it only in CI on that
# section's host. The report stays at REPORT for the workflow to upload.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COVERAGE_TARGET_DIR="${CARGO_TARGET_DIR:-target}/macos-logic-coverage"
REPORT="${COVERAGE_TARGET_DIR}/coverage-summary.json"

if [[ "$(uname -s)" != Darwin ]]; then
  printf 'macos-logic-coverage: the macOS-only floor needs a macOS host, not %s\n' "$(uname -s)" >&2
  exit 1
fi

# Profiles from an earlier source revision can under-report this tree, so the
# workspace's instrumented artifacts and profiles are cleared before every run;
# instrumented dependencies stay, since their profiles are not reported.
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov clean --workspace
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov -p sonicterm-ui --lib --tests \
  --json --summary-only --output-path "${REPORT}"

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
python3 "$ROOT/scripts/coverage-floor.py" macos-floor \
  --report "${REPORT}" \
  --baseline "$ROOT/scripts/coverage-baseline.json" \
  --target "${HOST_TARGET}" \
  --source-root "$ROOT"
