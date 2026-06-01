# sonicterm-shared

## 💀 DEPRECATED FAÇADE — dissolved at M7, removal target: v1.1

This crate is the legacy "shared" surface. At M7 the `render/` module
moves into its real owners (`sonicterm-render-model` + `sonicterm-text`
+ `sonicterm-gpu`); the `sonicterm-ui::*` re-exports stay as a thin
shim. After M7 every public item carries `#[deprecated]` (M9).

## Pre-M7 layout
- `sonicterm_ui::*` re-exports
- `render/` module split across
  `render/{mod,core,color,metrics,tab_spans,cursor,drag_chip}.rs`

## Post-M7 layout
- Thin re-export shim only. `render/*` lives in:
  - `sonicterm-render-model` (geometry / inputs / pure-data parts)
  - `sonicterm-text` (shaping + atlas glue)
  - `sonicterm-gpu` (pipeline + atlas upload)

## Tests
Until dissolution, hosts:
- `tests/render_capability_matrix.rs`
- `tests/snapshots/*.hash` (dHash visual baselines, gated by
  `bash scripts/check-visual-snapshots.sh`)

The matrix moves to `sonicterm-gpu` at M7; snapshots move with it.

## Owning PM(s)
- Primary: tag-owner during v1.1 release window (deletes the crate)

## Cross-references
- See `docs/migrations/0.9.0.md` for the leaf crate each item moved to
