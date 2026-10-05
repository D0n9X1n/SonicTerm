# sonicterm-text

## Purpose
Text shaping and glyph cache support for rendering. It owns shape caching,
row glyph caching, and glyph atlas data consumed by the GPU renderer.

## Key files
- `shape.rs` - shape cache and shaping entry points.
- `glyph_atlas.rs` - atlas pages and glyph placement.
- `row_glyph_cache.rs` - content-keyed row glyph cache: position-free records, pins, staged slots, quotas and budgets.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-text
```

## Guardrails
- Cache keys must account for font identity, size, weight, style, DPI, and
  glyph variants that change output.
- Avoid atlas allocation or eviction surprises on the hottest draw path.
- `RowGlyphCache` keys rows by content (SipHash with per-cache random keys; 0
  marks an empty slot), stores position-free `RowGlyph` records, and holds every
  public mutation within its two budgets: payload (448 MiB) by refusing
  admission, tracking (64 MiB) by leaving a pane untracked. A pane holds at most
  `4 × rows` entries and `4 × rows × cols` cells of payload; eviction removes only
  that pane's unpinned rows, oldest first, to three quarters of each quota.
  Payload is measured by `cached_row_payload_bytes` after shrinking, and every
  running sum and report uses that one function.
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
- `START_ATLAS_DIM_1X` and `START_ATLAS_DIM_2X` stay 2048: normal renderers
  keep the full allocation, and only warm spares start at `MIN_ATLAS_DIM`.
  `start_size_inputs::validate_table_start` rejects any smaller start while
  `SIZING_ORACLE_COMPLETE` is false, whatever rows `START_SIZE_INPUTS` holds,
  and a row with nonzero `incomplete_glyphs` always selects 2048. The guard
  stays false until the real renderer reports silently skipped shaped-glyph
  raster or admission failures and failed tab-title fitting as missing glyphs.
- Eviction is what keeps the index bounded. With eviction disabled the index
  still stops growing, because a full atlas stops admitting — memory looks
  flat while every later glyph goes missing. Assert that eviction ran, not
  only that memory stayed bounded.
- Keep shaping/raster behavior aligned with `sonicterm-font`; do not add
  vendor font dependencies.

## Cross-references
- Consumes: `sonicterm-types` plus external headless text/image utilities.
- Consumed by: `sonicterm-engine`, `sonicterm-gpu`, `sonicterm-app`.
