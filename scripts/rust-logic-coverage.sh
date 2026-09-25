#!/usr/bin/env bash
set -euo pipefail

# Release coverage gate for deterministic Rust logic, plus a per-crate report and
# regression floor over every workspace member. The 80% subset gate leaves out
# native window/GPU/PTY paths, generated FFI, platform font discovery, and large
# renderer-facing controllers, which use their build, integration, and
# release-smoke substitutes. Exact crate roots keep app-core and font-config in
# the measured surface while the audited cfg/grid/render/type/UI/font
# helpers remain visible. The same profiles then feed scripts/coverage-floor.py,
# which prints line coverage for every member and holds each measured crate to
# its reviewed floor in scripts/coverage-baseline.json.
# Separators are written [/\\] so the pattern matches both the POSIX paths
# llvm-cov reports on macOS and Linux and the backslash paths it reports on
# Windows. A one-sided pattern silently matches nothing on the other platform,
# which leaves the excluded native paths in the measured surface instead of
# filtering them out.
#
# Once this script starts, it leaves evidence for the macos-coverage job to
# upload after success and after failure. It pins the checkout before its first
# record, rewrites a provenance record in EVIDENCE_DIR when each phase starts
# and when one fails, and stages the JSON report and the workspace inventory.
# The publish phase refuses a checkout whose HEAD, tree, or list of uncommitted
# coverage-relevant paths differs from the pin, then renames the staged
# directory into place, so both files appear together or not at all, before the
# 80% subset gate or the floor can fail the run; the script still exits with the
# failing status. Only a complete record whose digests match vouches for them.
IGNORE_REGEX='crates[/\\](sonicterm-(app|gpu|mac|windows|logging|io|engine|block-glyph|harfbuzz|fontconfig)[/\\]|sonicterm-cfg[/\\]src[/\\](config|keymap|lib)\.rs|sonicterm-render-model[/\\]src[/\\](lib|painter|pane_render)\.rs|sonicterm-font[/\\]src[/\\](db|fcwrap|ftwrap|hbwrap|lib)\.rs|sonicterm-font[/\\]src[/\\](locator|rasterizer|shaper)[/\\]|sonicterm-freetype[/\\](src[/\\](lib|types)\.rs|build\.rs)|sonicterm-text[/\\]src[/\\](lib|glyph_atlas|row_glyph_cache)\.rs|sonicterm-types[/\\]src[/\\](action|geom|glyph_key|hyperlink_id|lib|window_key)\.rs|sonicterm-types[/\\]src[/\\]traits[/\\]|sonicterm-ui[/\\]src[/\\](broadcast|command_palette|cursor|i18n|ime|overlays|pane|scrollbar|search|selection|ui_tokens)\.rs|[/\\]build\.rs$)'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COVERAGE_TARGET_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage"
EVIDENCE_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-evidence"
WORK_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage-work"
STAGE_DIR="${WORK_DIR}/measurement-staging"
MEASUREMENT_DIR="${EVIDENCE_DIR}/measurement"
REPORT="${MEASUREMENT_DIR}/coverage-summary.json"
INVENTORY="${MEASUREMENT_DIR}/workspace-metadata.json"
RECORD="${EVIDENCE_DIR}/coverage-provenance.json"
PIN="${WORK_DIR}/checkout-pin.json"
FLOOR_TOOL="$ROOT/scripts/coverage-floor.py"
PY=""
COVERAGE_HOST_TARGET=""
RUSTC_VERSION_INFO=""
LLVM_COV_VERSION=""
PHASE=""

# Cached Cargo/llvm-cov artifacts can retain profile mappings from earlier
# source revisions and under-report the current tree, and stale evidence would
# describe another run. Every directory is removed before every run,
# independent of CI's restored workspace target cache.
rm -rf "${COVERAGE_TARGET_DIR}" "${EVIDENCE_DIR}" "${WORK_DIR}"
mkdir -p "${EVIDENCE_DIR}" "${STAGE_DIR}"

# Write the provenance record for a phase and state. The report and inventory
# exist only after the publish rename, so their digests appear only once both
# are in place.
record() {
  local phase="$1" state="$2" status="${3:-0}"
  local files=()
  if [[ -f "${REPORT}" && -f "${INVENTORY}" ]]; then
    files=(--report "${REPORT}" --metadata "${INVENTORY}")
  fi
  "$PY" "${FLOOR_TOOL}" record-provenance --output "${RECORD}" --root "${ROOT}" --pin "${PIN}" \
    --phase "${phase}" --state "${state}" --exit-status "${status}" \
    --target "${COVERAGE_HOST_TARGET}" --rustc-verbose "${RUSTC_VERSION_INFO}" \
    --llvm-cov "${LLVM_COV_VERSION}" ${files[@]+"${files[@]}"}
}

# Start a phase once its write-ahead record exists. A failure to write that
# record keeps the previous one and leaves no phase for the exit trap to blame.
# Before subset-gate begins, the kept record marks the run incomplete; during
# subset-gate or floor, it can describe a complete measurement whose check is
# not finished.
begin() {
  PHASE=""
  record "$1" running
  PHASE="$1"
}

# A failed phase rewrites its write-ahead record with the exit status. If no
# record could be written at all, a minimal one still marks the run incomplete.
on_exit() {
  local status=$?
  if [[ ${status} -ne 0 && -n "${PHASE}" ]]; then
    record "${PHASE}" failed "${status}" || true
  fi
  if [[ ${status} -ne 0 && ! -f "${RECORD}" ]]; then
    printf '{"schema": "sonicterm-coverage-provenance/1", "measurement": "incomplete", "failed_phase": "%s", "failure": "exit status %s; no provenance record could be written"}\n' \
      "${PHASE:-self-test}" "${status}" > "${RECORD}" || true
  fi
  exit "${status}"
}
trap on_exit EXIT

# The self-test phase finds Python, pins the checkout, and tests the floor logic
# before the instrumented build, so a comparison bug fails in seconds instead of
# after the build and cannot turn into a green floor. The pin fixes the commit,
# tree, path map, and uncommitted changes that every later record carries.
PHASE=self-test
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  printf 'python3 or python is required for the per-crate coverage floor\n' >&2
  exit 1
fi
"$PY" "${FLOOR_TOOL}" pin-checkout --root "${ROOT}" --output "${PIN}"
record self-test running
"$PY" -m py_compile "${FLOOR_TOOL}" "$ROOT/scripts/coverage-floor_tests.py"
(
  cd "$ROOT/scripts"
  "$PY" coverage-floor_tests.py
)

# The baseline is keyed to the host target of the toolchain that builds the
# instrumented tests; fail before the build when rustc cannot report it.
begin toolchain
RUSTC_VERSION_INFO="$(rustc -vV)"
COVERAGE_HOST_TARGET="$(printf '%s\n' "${RUSTC_VERSION_INFO}" | sed -n 's/^host: //p')"
COVERAGE_RUSTC="$(printf '%s\n' "${RUSTC_VERSION_INFO}" | sed -n '1p')"
if [[ -z "${COVERAGE_HOST_TARGET}" ]]; then
  printf 'rustc -vV reported no host target\n' >&2
  exit 1
fi
# Diagnostic for an incomplete run only: a complete record takes the version
# from the report, so a failed lookup is recorded as unknown instead of failing.
LLVM_COV_VERSION="$(cargo llvm-cov --version 2>/dev/null || true)"

begin instrumented-tests
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov --workspace --lib --bins --tests --no-report

# Report the profiles without the ignore regex and without rebuilding. The
# workspace inventory names every member and build script, so a member that
# silently drops out of the report fails the floor instead of vanishing. Both
# files are staged outside the uploaded directory.
begin report
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov report --json --summary-only \
  --output-path "${STAGE_DIR}/coverage-summary.json"
begin inventory
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo metadata --no-deps --format-version 1 \
  > "${STAGE_DIR}/workspace-metadata.json"

# The publish record refuses, and names, a checkout whose HEAD, tree, or list of
# uncommitted coverage-relevant paths differs from its pin. It compares names,
# not contents, and cannot see a change reverted before it runs. One rename then
# moves the staged directory into place: the report and inventory appear
# together or not at all. A rename across filesystems fails instead of copying.
begin publish
"$PY" -c 'import os, sys; os.rename(sys.argv[1], sys.argv[2])' "${STAGE_DIR}" "${MEASUREMENT_DIR}"

# The 80% subset gate and the floor judge the published measurement, so either
# can fail the run without losing it.
begin subset-gate
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov report \
  --ignore-filename-regex "${IGNORE_REGEX}" \
  --fail-under-lines 80

begin floor
"$PY" "${FLOOR_TOOL}" \
  --report "${REPORT}" \
  --metadata "${INVENTORY}" \
  --baseline "$ROOT/scripts/coverage-baseline.json" \
  --target "${COVERAGE_HOST_TARGET}" \
  --toolchain "${COVERAGE_RUSTC}"
# The run is finished; a failure to write the final record keeps the floor's
# write-ahead record instead of blaming the floor.
PHASE=""
record floor "done"
