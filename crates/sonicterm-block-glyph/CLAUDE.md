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
- `LICENSE-WEZTERM` - attribution for imported custom glyph code.

## Local gate
```bash
cargo build -p sonicterm-block-glyph
```

## Guardrails
- Keep pixel-unit conversions in `glue.rs` or `lib.rs`; do not leak them
  into the renderer.
- Preserve attribution headers when touching imported code.

## Cross-references
- Consumes: `sonicterm-engine`, `sonicterm-cfg`.
- Consumed by: font/text/rendering code that needs block glyph sprites.
