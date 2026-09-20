#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/crates/sonicterm-harfbuzz"

BINDGEN="${BINDGEN:-bindgen}"
test "$("$BINDGEN" --version)" = "bindgen 0.71.1" || {
  printf 'Install bindgen-cli 0.71.1 with --locked before regenerating bindings.\n' >&2
  exit 1
}

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
rustc --edition=2021 "$ROOT/scripts/freetype-config.rs" -o "$scratch/freetype-config"
"$scratch/freetype-config" ../sonicterm-freetype/freetype2/include/freetype/config/ftoption.h "$scratch/include"

"$BINDGEN" bindings.h -o src/lib.rs \
  --rust-target 1.82 \
  --wrap-unsafe-ops \
  --no-layout-tests \
  --no-doc-comments \
  --raw-line "#![allow(non_snake_case)]" \
  --raw-line "#![allow(non_camel_case_types)]" \
  --raw-line "#![allow(non_upper_case_globals)]" \
  --raw-line "#![allow(clippy::unreadable_literal)]" \
  --raw-line "#![allow(clippy::upper_case_acronyms)]" \
  --raw-line '#[cfg(test)] #[path = "lib_tests.rs"] mod lib_tests;' \
  --default-enum-style rust \
  --generate=functions,types,vars \
  --allowlist-function="hb_.*" \
  --allowlist-type="hb_.*" \
  -- -Iharfbuzz/src -I"$scratch/include" -I../sonicterm-freetype/freetype2/include
