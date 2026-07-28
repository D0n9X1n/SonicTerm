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
- Scrollback is bounded twice: by the configured row count and by retained
  bytes. Rows carrying hyperlinks, combining marks, or non-default underlines
  cost more than plain text, so the byte budget can bite first. Enforcement
  runs on the scroll path, amortized — keep it amortized, since the check
  walks every row and per-scroll would make a long `cat` quadratic.
- The hyperlink registry reclaims on fill rather than per link. Sweeping the
  grid on every OSC 8 would be quadratic; retention is therefore a sawtooth,
  not a flat line, and a test asserting a flat line is asserting the wrong
  shape.

## Cross-references
- Consumes: `sonicterm-types`.
- Consumed by: `sonicterm-vt`, `sonicterm-app`, `sonicterm-ui`,
  `sonicterm-render-model`, `sonicterm-engine`.
