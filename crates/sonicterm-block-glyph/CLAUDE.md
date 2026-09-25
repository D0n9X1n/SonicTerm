# sonicterm-block-glyph

## Purpose
Rasterizes terminal block/box drawing glyphs with SonicTerm cell metrics.
This is the Sonic-owned boundary for the custom glyph code imported from
WezTerm; callers should use this crate instead of reaching into vendor
paths.

## Key files
- `customglyph.rs` - block glyph rasterization logic.
- `glue.rs` - SonicTerm bitmap, color, point, rect, and metric adapters.
- `lib.rs` - public `block_sprite_with_cell_metrics` wrapper.
- `customglyph_tests.rs` - data-free geometry invariants and the mapped-codepoint sweep.
- `lib_tests.rs` - shared raster case table, isolated-texel fixture test, and the
  reviewed digest test with its bless path.
- `raster-digests.golden.tsv` - reviewed raster digests; changes only through bless.
- `LICENSE-WEZTERM` - attribution for imported custom glyph code.

## Local gate
```bash
cargo test -p sonicterm-block-glyph
```

Regenerate the reviewed digest table only for a named geometry change:
`SONICTERM_BLESS_BLOCK_GLYPH=1 cargo test -p sonicterm-block-glyph raster_digests`
rewrites it and fails; review the diff, then rerun without the variable. The
procedure is in `wiki/Development-and-Release.md`, "Reviewed block-glyph rasters".

## Guardrails
- Keep pixel-unit conversions in `glue.rs` or `lib.rs`; do not leak them
  into the renderer.
- Preserve attribution headers when touching imported code.
- Never bless a digest change without a named geometry change and its printed
  rasters. Derive invariant thresholds from geometry, not from measured output.

## Cross-references
- Consumes: no first-party SonicTerm crate; the public bitmap/metric boundary is
  intentionally leaf-tight.
- Consumed by: `sonicterm-gpu`.
