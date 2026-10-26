#!/usr/bin/env bash
set -euo pipefail

# Coverage floor for the macOS-only logic inside the 80% subset. Logic coverage
# runs on Linux, where the AppKit word boundary in sonicterm-ui's text_edit.rs
# and the macOS arms of command_label.rs compile out, so the Linux crate floors
# cannot see them. This script instruments sonicterm-ui alone on macOS and holds
# each file in the baseline's `macos` section to its floor through
# `coverage-floor.py macos-floor`, which enforces it only in CI on that
# section's host.
#
# It pins the checkout before measuring, and after the measurement writes a
# provenance record beside the report in EVIDENCE_DIR: the run, host,
# toolchain, cargo-llvm-cov, commit, tree and the report's SHA-256. The workflow
# uploads both, and only that record, verified, can change the macos section.
# A checkout that drifted from its pin during the measurement fails the run
# after the record names the drift.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COVERAGE_TARGET_DIR="${CARGO_TARGET_DIR:-target}/macos-logic-coverage"
EVIDENCE_DIR="${CARGO_TARGET_DIR:-target}/macos-logic-coverage-evidence"
WORK_DIR="${CARGO_TARGET_DIR:-target}/macos-logic-coverage-work"
REPORT="${EVIDENCE_DIR}/coverage-summary.json"
RECORD="${EVIDENCE_DIR}/coverage-provenance.json"
PIN="${WORK_DIR}/checkout-pin.json"
FLOOR_TOOL="$ROOT/scripts/coverage-floor.py"

if [[ "$(uname -s)" != Darwin ]]; then
  printf 'macos-logic-coverage: the macOS-only floor needs a macOS host, not %s\n' "$(uname -s)" >&2
  exit 1
fi

# Stale evidence would describe another run, so the evidence and the pin start empty.
rm -rf "${EVIDENCE_DIR}" "${WORK_DIR}"
mkdir -p "${EVIDENCE_DIR}" "${WORK_DIR}"
python3 "${FLOOR_TOOL}" pin-checkout --root "$ROOT" --output "${PIN}"

# Profiles from an earlier source revision can under-report this tree, so the
# workspace's instrumented artifacts and profiles are cleared before every run;
# instrumented dependencies stay, since their profiles are not reported.
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov clean --workspace
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov -p sonicterm-ui --lib --tests \
  --json --summary-only --output-path "${REPORT}"

RUSTC_VERBOSE="$(rustc -vV)"
HOST_TARGET="$(printf '%s\n' "${RUSTC_VERBOSE}" | sed -n 's/^host: //p')"
python3 "${FLOOR_TOOL}" record-macos-provenance --output "${RECORD}" --root "$ROOT" --pin "${PIN}" \
  --target "${HOST_TARGET}" --rustc-verbose "${RUSTC_VERBOSE}" --report "${REPORT}"
python3 "${FLOOR_TOOL}" macos-floor \
  --report "${REPORT}" \
  --baseline "$ROOT/scripts/coverage-baseline.json" \
  --target "${HOST_TARGET}" \
  --source-root "$ROOT"
