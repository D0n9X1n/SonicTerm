# sonicterm-text

## Purpose
Text shaping data and glyph cache support for rendering. It owns the shaping
run model, row glyph caching, and glyph atlas data consumed by the GPU renderer.
It holds no shape-result cache.

## Key files
- `shape.rs` - the ASCII fast-path predicate (generic over owned or borrowed cells), `RunStyle` and `ShapedGlyph`.
- `glyph_atlas.rs` - atlas pages and glyph placement.
- `row_glyph_cache.rs` - content-keyed row glyph cache: position-free records, pins, staged slots, quotas and budgets.
- `face_content.rs` - a face's content identity: the namespaced SHA-256 of the bytes it was loaded from.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-text
```

## Guardrails
- `RowGlyphCache::release_all` frees every pane record and the tables but keeps the hasher, budgets and frame clock, so the same row keeps its content key after a release.
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
- `START_ATLAS_DIM_1X` is 1024 and `START_ATLAS_DIM_2X` is 2048, each equal to
  `start_size_inputs::ruled_start` over `START_SIZE_INPUTS` at its scale; a unit
  test checks the equality, because `validate_table_start` accepts the maximum
  unconditionally. Warm spares start at `MIN_ATLAS_DIM`. The table holds 64 CI
  rows, each with its `Provenance` (run, attempt, measured SHA, side, set and
  origin): 32 perf-end rows from a Performance comparison's base side, 8 helper
  rows and 24 Windows real-renderer rows from one push CI run, kept apart by
  `source`. `SIZING_ORACLE_COMPLETE` is true; `ruled_start` still refuses a
  scale missing any `required_inputs` row, and a row with nonzero
  `incomplete_glyphs` always selects 2048. Changing a start constant means new
  rows and the rule's new answer, never an edited constant alone. `RASTER_EXCEPTIONS` (empty) exempts a raster
  failure only by exact platform, role, face content (`face_content`: the
  namespaced SHA-256 of the face's bytes, never its file name) and index, glyph
  id, style and strike, with a reviewed reason; `incomplete_glyphs` counts unresolved
  characters, oversize required tiles and unapproved raster failures.
- Eviction is what keeps the index bounded. With eviction disabled the index
  still stops growing, because a full atlas stops admitting — memory looks
  flat while every later glyph goes missing. Assert that eviction ran, not
  only that memory stayed bounded.
- Keep shaping/raster behavior aligned with `sonicterm-font`; do not add
  vendor font dependencies.

## Cross-references
- Consumes: `sonicterm-types` plus external headless text/image utilities.
- Consumed by: `sonicterm-engine`, `sonicterm-gpu`, `sonicterm-app`.
