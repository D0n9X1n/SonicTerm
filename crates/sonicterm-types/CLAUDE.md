# sonicterm-types

## Purpose
Zero-dependency contracts shared across crates: cells, geometry, actions,
modifier keys, glyph keys, window keys, hyperlink IDs, active trait seams for
window/clipboard/PTY boundaries, and the legacy painter compatibility seam.

## Key files
- `cell.rs`, `geom.rs`, `glyph_key.rs`, `hyperlink_id.rs` - value types.
- `action.rs`, `mod_key.rs`, `window_key.rs` - input/action contracts.
- `traits/` - window, clipboard, and PTY seams plus legacy painter compatibility types.
- `lib.rs` - crate-level exports.

## Local gate
```bash
cargo test -p sonicterm-types
```

## Guardrails
- Keep this crate dependency-light and backend-free.
- Public API changes require a manual review of the cross-crate boundary in
  `Architecture-Internals` and updates to affected crate/user documentation.
  No snapshot, script, or CI job records or diffs the public API.
- Prefer small value types and explicit trait seams over leaking app,
  renderer, or platform types.

## Cross-references
- Consumed by: nearly every SonicTerm crate.
