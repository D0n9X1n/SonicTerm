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

## Cross-references
- Consumes: `sonicterm-grid`, `sonicterm-types`.
- Consumed by: `sonicterm-app`, `sonicterm-mux`.
