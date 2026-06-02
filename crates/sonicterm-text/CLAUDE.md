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
The render-regression rule (CLAUDE.md §11) applies: any change here
MUST pass the capability matrix:
```bash
cargo test -p sonicterm-core --test vt_capability_matrix
cargo test -p sonicterm-text --test render_capability_matrix
cargo run --example pty_dump_unicode -p sonicterm-core --release
```
PR #42 shipped CJK tofu past the local gate because every test was
ASCII-only — do NOT delete or weaken the matrix.

Render hot-file rule (closes #283): changes to `glyph_atlas.rs` or
`swash_rasterizer.rs` MUST keep `bash scripts/check-visual-snapshots.sh`
green OR bump the dHash baselines in the same PR (`UPDATE_SNAPSHOTS=1`,
commit refreshed `crates/sonicterm-shared/tests/snapshots/*.hash`,
append a row to README).

## Test gate (local)
```bash
cargo test -p sonicterm-text
cargo test -p sonicterm-core --test vt_capability_matrix
cargo test -p sonicterm-text --test render_capability_matrix
cargo test -p sonicterm-text --test symbol_fit         # PR #456 SymbolFit policy
cargo test -p sonicterm-text --test font_coverage_pua  # PR #453 PUA coverage
cargo run --example pty_dump_unicode -p sonicterm-core --release
bash scripts/check-visual-snapshots.sh
# Plus §13 GUI smoke (mac) — see crates/sonicterm-app/CLAUDE.md
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
