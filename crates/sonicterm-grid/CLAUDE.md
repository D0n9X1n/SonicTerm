# sonicterm-grid

## Purpose
Terminal grid + scrollback. Owns the cell buffer, wide-char layout,
alt-screen toggle, dirty bitset, and hyperlink registry. The shared
mutable state between the parser and the renderer.

## Public surface
- `grid::Grid` (cells, scrollback, wide chars, alt screen)
- `hyperlink::HyperlinkRegistry`

## Land-mines specific to this crate
None named in §4, but the grid is the AB-BA partner in LM-001:
the renderer takes the parser lock, the parser writes the grid. If you
change locking discipline here, see LM-001 in `sonicterm-app/CLAUDE.md`.

## Test gate (local)
```bash
cargo build -p sonicterm-grid
```

## Common pitfalls
- Dirty-bitset off-by-one when the cursor crosses the alt-screen boundary
- Wide-char placement: second half-cell is `Cell::continuation`, not blank
- Scrollback bottom != cursor row when alt-screen is active

## Owning PM(s)
- Primary: either (cross-platform pure-data)
- Hot-file: yes (grid.rs)

## Cross-references
- Consumes traits from: `sonicterm-types`
- Consumed by: `sonicterm-vt` (Performer), `sonicterm-app`, `sonicterm-render-model`
