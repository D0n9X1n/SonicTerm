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
IGNORE_REGEX='crates[/\\](sonicterm-(app|gpu|mac|windows|logging|io|engine|block-glyph|harfbuzz|fontconfig)[/\\]|sonicterm-cfg[/\\]src[/\\](config|keymap|lib)\.rs|sonicterm-render-model[/\\]src[/\\](lib|painter|pane_render)\.rs|sonicterm-font[/\\]src[/\\](db|fcwrap|ftwrap|hbwrap|lib)\.rs|sonicterm-font[/\\]src[/\\](locator|rasterizer|shaper)[/\\]|sonicterm-freetype[/\\](src[/\\](lib|types)\.rs|build\.rs)|sonicterm-text[/\\]src[/\\](lib|glyph_atlas|row_glyph_cache)\.rs|sonicterm-types[/\\]src[/\\](action|geom|glyph_key|hyperlink_id|lib|window_key)\.rs|sonicterm-types[/\\]src[/\\]traits[/\\]|sonicterm-ui[/\\]src[/\\](broadcast|command_palette|cursor|i18n|ime|overlays|pane|scrollbar|search|selection|ui_tokens)\.rs|[/\\]build\.rs$)'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  printf 'python3 or python is required for the per-crate coverage floor\n' >&2
  exit 1
fi

# Test the floor logic before the instrumented build, so a comparison bug fails
# in seconds instead of after the build and cannot turn into a green floor.
"$PY" -m py_compile "$ROOT/scripts/coverage-floor.py" "$ROOT/scripts/coverage-floor_tests.py"
(
  cd "$ROOT/scripts"
  "$PY" coverage-floor_tests.py
)

# The baseline is keyed to the host target of the toolchain that builds the
# instrumented tests; fail before the build when rustc cannot report it.
RUSTC_VERSION_INFO="$(rustc -vV)"
COVERAGE_HOST_TARGET="$(printf '%s\n' "${RUSTC_VERSION_INFO}" | sed -n 's/^host: //p')"
COVERAGE_RUSTC="$(printf '%s\n' "${RUSTC_VERSION_INFO}" | sed -n '1p')"
if [[ -z "${COVERAGE_HOST_TARGET}" ]]; then
  printf 'rustc -vV reported no host target\n' >&2
  exit 1
fi

# Cached Cargo/llvm-cov artifacts can retain profile mappings from earlier
# source revisions and under-report the current tree. Build coverage in a
# dedicated directory that is removed before every run, independent of CI's
# restored workspace target cache.
COVERAGE_TARGET_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage"
rm -rf "${COVERAGE_TARGET_DIR}"
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov --workspace --lib --bins --tests \
  --ignore-filename-regex "${IGNORE_REGEX}" \
  --fail-under-lines 80

# Re-report the same profiles without the ignore regex and without rebuilding.
# The workspace inventory names every member and build script, so a member
# that silently drops out of the report fails the floor instead of vanishing.
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov report --json --summary-only \
  --output-path "${COVERAGE_TARGET_DIR}/coverage-summary.json"
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo metadata --no-deps --format-version 1 \
  > "${COVERAGE_TARGET_DIR}/workspace-metadata.json"

"$PY" "$ROOT/scripts/coverage-floor.py" \
  --report "${COVERAGE_TARGET_DIR}/coverage-summary.json" \
  --metadata "${COVERAGE_TARGET_DIR}/workspace-metadata.json" \
  --baseline "$ROOT/scripts/coverage-baseline.json" \
  --target "${COVERAGE_HOST_TARGET}" \
  --toolchain "${COVERAGE_RUSTC}"
