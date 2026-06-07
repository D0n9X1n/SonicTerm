# sonicterm-types

## Purpose
Zero-dependency contracts shared across crates: cells, geometry, actions,
modifier keys, glyph keys, window keys, hyperlink IDs, and trait seams for
painter/window/clipboard/PTY boundaries.

## Key files
- `cell.rs`, `geom.rs`, `glyph_key.rs`, `hyperlink_id.rs` - value types.
- `action.rs`, `mod_key.rs`, `window_key.rs` - input/action contracts.
- `traits/` - painter, window, clipboard, and PTY seams.
- `lib.rs` - crate-level exports.

## Local gate
```bash
cargo test -p sonicterm-types
```

## Guardrails
- Keep this crate dependency-light and backend-free.
- Public API changes require `docs/CONTRACTS.md` updates and the documented
  deprecation/migration protocol.
- Prefer small value types and explicit trait seams over leaking app,
  renderer, or platform types.

## Cross-references
- Consumed by: nearly every SonicTerm crate.
