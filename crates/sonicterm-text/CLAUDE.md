# sonicterm-text

## Purpose
Text shaping and glyph cache support for rendering. It owns shape caching,
row glyph caching, and glyph atlas data consumed by the GPU renderer.

## Key files
- `shape.rs` - shape cache and shaping entry points.
- `glyph_atlas.rs` - atlas pages and glyph placement.
- `row_glyph_cache.rs` - row-level glyph cache.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-text
```

## Guardrails
- Cache keys must account for font identity, size, weight, style, DPI, and
  glyph variants that change output.
- Avoid atlas allocation or eviction surprises on the hottest draw path.
- The atlas is a fixed-size texture plus an index. `retained_amount().bytes`
  is the texture capacity and is constant by construction; only `items`
  moves. A test bounding `bytes` compares a constant to itself and would
  pass against any defect.
- Eviction is what keeps the index bounded. With eviction disabled the index
  still stops growing, because a full atlas stops admitting — memory looks
  flat while every later glyph goes missing. Assert that eviction ran, not
  only that memory stayed bounded.
- Keep shaping/raster behavior aligned with `sonicterm-font`; do not add
  vendor font dependencies.

## Cross-references
- Consumes: `sonicterm-types` plus external headless text/image utilities.
- Consumed by: `sonicterm-ui`, `sonicterm-engine`, `sonicterm-gpu`,
  `sonicterm-app`.
