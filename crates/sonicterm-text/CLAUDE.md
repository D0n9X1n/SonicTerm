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

Render-touching changes should be visually smoke-checked.

## Guardrails
- Cache keys must account for font identity, size, weight, style, DPI, and
  glyph variants that change output.
- Avoid atlas allocation or eviction surprises on the hottest draw path.
- Keep shaping/raster behavior aligned with `sonicterm-font`; do not add
  vendor font dependencies.

## Cross-references
- Consumes: `sonicterm-font`, `sonicterm-types`.
- Consumed by: `sonicterm-gpu`.
