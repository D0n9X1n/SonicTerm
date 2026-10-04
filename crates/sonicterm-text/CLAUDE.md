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
- UV-bearing caches use `GlyphAtlas::identity()`, not the resettable eviction
  counter, and still clear promptly when their owning seam changes.
- A `GlyphAtlas::growable` atlas doubles up to its maximum before it evicts;
  `new` and `default_size` stay fixed. `grow_to` accepts only the policy's
  next doubling and refuses anything else without changing the atlas. Growth
  copies resident tiles, keeps their positions, recomputes UVs, queues one
  typed re-upload rect per tile, advances the identity and counts one growth
  that a reset does not clear; it never rasterizes again.
  `retained_amount().bytes` is the pixel capacity plus the dirty list's
  capacity. Pixels move only with growth; the dirty list grows with inserts
  and a drain shrinks it to 64 rects once it exceeds 1,024, so even a fixed
  atlas's figure varies. A test bounding memory must assert the term it means.
- Eviction is what keeps the index bounded. With eviction disabled the index
  still stops growing, because a full atlas stops admitting — memory looks
  flat while every later glyph goes missing. Assert that eviction ran, not
  only that memory stayed bounded.
- Keep shaping/raster behavior aligned with `sonicterm-font`; do not add
  vendor font dependencies.

## Cross-references
- Consumes: `sonicterm-types` plus external headless text/image utilities.
- Consumed by: `sonicterm-engine`, `sonicterm-gpu`, `sonicterm-app`.
