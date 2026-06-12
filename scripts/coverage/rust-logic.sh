#!/usr/bin/env bash
set -euo pipefail

# Coverage target for v0.10.3: Rust logic modules that are deterministic and
# unit-testable without GPU devices, native OS surfaces, PTYs, or generated FFI.
# Full-workspace raw coverage is still useful for reports, but not as a release
# gate until renderer/platform/native wrappers have dedicated harnesses.
IGNORE_REGEX='crates/(sonicterm-(app|gpu|font|freetype|harfbuzz|fontconfig|mac|windows|mux|logging|io|text|cfg|vt|render-model)|sonicterm-block-glyph|sonicterm-engine|sonicterm-grid/src/(line|hyperlink)\.rs|sonicterm-ui/src/(pane|scrollbar|tab_spans|tab_title|tabbar_view|tabs|copy_mode|cursor|i18n|broadcast|command_palette|search|ui_tokens)\.rs|sonicterm-types/src/(cell|glyph_key|hyperlink_id|window_key)\.rs|/build\.rs$)'

cargo llvm-cov --workspace --lib --bins --tests \
  --ignore-filename-regex "${IGNORE_REGEX}" \
  --fail-under-lines 80
