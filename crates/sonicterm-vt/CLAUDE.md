# sonicterm-vt

## Purpose
VT/ANSI parser and terminal-state mutation layer. It decodes PTY bytes,
handles escape/control sequences, applies modes/styles, and mutates the
grid through the terminal model.

## Key files
- `vt.rs` - parser, control sequence handling, terminal state mutation.
- `lib.rs` - public exports.

## Local gate
```bash
cargo test -p sonicterm-vt
```

## Guardrails
- Preserve SWAR/ASCII fast paths when changing parser hot loops.
- Do not flatten styled rows in ways that lose per-cell foreground,
  background, inverse, underline, or hyperlink data.
- Parser changes affect rendering and input semantics; add targeted tests
  for escape-sequence regressions.
- Keep PTY writes outside parser/grid locks.
- Media capture staging is drawn from a process-wide pool, not per-parser.
  A capture that cannot be staged is refused and renders nothing; do not add
  a path that stages unconditionally or renders a partial payload. A cut
  Sixel is byte-identical to a complete short one, so a partial render is
  indistinguishable from correct output.
- `cancel_capture` is called by the host on a stalled transfer, and the
  sender is never told. It must leave the parser swallowing the remaining
  payload rather than returning to ground — a payload is printable ASCII
  end to end, so a parser back in ground prints it into the grid.

## Cross-references
- Consumes: `sonicterm-grid`, `sonicterm-types`.
- Consumed by: `sonicterm-app`.
