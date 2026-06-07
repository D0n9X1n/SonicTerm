# sonicterm-grid

## Purpose
Terminal cell storage: visible grid, scrollback, line metadata, dirty
tracking, wide-character handling, and hyperlink references.

## Key files
- `grid.rs` - grid mutation, scrollback, resize, dirty tracking.
- `line.rs` - row storage and cell/span helpers.
- `hyperlink.rs` - hyperlink metadata.
- `lib.rs` - public exports.

## Local gate
```bash
cargo test -p sonicterm-grid
```

## Guardrails
- Wide and zero-width characters must keep cells visually and logically
  aligned after resize/scroll.
- Dirty tracking should be precise; do not mark the whole grid dirty for
  narrow updates unless unavoidable.
- Preserve scrollback invariants when changing erase, scroll, or resize.

## Cross-references
- Consumes: `sonicterm-types`.
- Consumed by: `sonicterm-vt`, `sonicterm-app`, `sonicterm-mux`,
  `sonicterm-render-model`.
