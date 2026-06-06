# sonicterm-text

## Purpose
Shaping + atlas. LRU shape cache, swash rasterizer, glyph atlas, per-row
glyph cache. Hot path for every frame.

## Public surface
- `shape` (LRU shape cache)
- `swash_rasterizer`
- `glyph_atlas`
- `row_glyph_cache`

## Land-mines specific to this crate
Render hot-file rule (closes #283): changes to `glyph_atlas.rs` or
`swash_rasterizer.rs` must keep the app/mac build green and should be
smoke-checked visually.

## Test gate (local)
```bash
cargo build -p sonicterm-text
```

## Common pitfalls
- "Primary family only, no fallback" path — see PR #42 postmortem
- Atlas page eviction races: cache key must include weight + style
- HiDPI blur if `set_metrics` isn't fed the device pixel ratio

## Owning PM(s)
- Primary: either; §13 smoke required from BOTH PMs
- Hot-file: yes (render-touching)

## Cross-references
- Consumes traits from: `sonicterm-types::Painter`
- Consumed by: `sonicterm-gpu` directly; legacy `sonicterm-shared::render` shim re-exports for back-compat.
