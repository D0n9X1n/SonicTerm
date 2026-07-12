#!/usr/bin/env bash
set -euo pipefail

# Release coverage gate for deterministic Rust logic selected by the v1.1.0
# audit. Native window/GPU/PTY paths, generated FFI, platform font discovery,
# and large renderer-facing controllers use their build, integration, and
# release-smoke substitutes. Exact crate roots keep app-core and font-config in
# the measured surface while the audited cfg/grid/render/type/UI/font/mux
# helpers remain visible.
IGNORE_REGEX='crates/(sonicterm-(app|gpu|mac|windows|logging|io|engine|block-glyph|harfbuzz|fontconfig|vt)/|sonicterm-cfg/src/(config|keymap|lib)\.rs|sonicterm-render-model/src/(lib|painter|pane_render)\.rs|sonicterm-font/src/(db|fcwrap|ftwrap|hbwrap|lib)\.rs|sonicterm-font/src/(locator|rasterizer|shaper)/|sonicterm-freetype/(src/(lib|types)\.rs|build\.rs)|sonicterm-mux/src/(lib|main|server)\.rs|sonicterm-text/src/(lib|glyph_atlas|row_glyph_cache)\.rs|sonicterm-types/src/(action|geom|glyph_key|hyperlink_id|lib|window_key)\.rs|sonicterm-types/src/traits/|sonicterm-ui/src/(broadcast|command_palette|cursor|i18n|ime|overlays|pane|scrollbar|search|selection|ui_tokens)\.rs|/build\.rs$)'

# Cached Cargo/llvm-cov artifacts can retain profile mappings from earlier
# source revisions and under-report the current tree. Build coverage in a
# dedicated directory that is removed before every run, independent of CI's
# restored workspace target cache.
COVERAGE_TARGET_DIR="${CARGO_TARGET_DIR:-target}/rust-logic-coverage"
rm -rf "${COVERAGE_TARGET_DIR}"
CARGO_TARGET_DIR="${COVERAGE_TARGET_DIR}" cargo llvm-cov --workspace --lib --bins --tests \
  --ignore-filename-regex "${IGNORE_REGEX}" \
  --fail-under-lines 80
