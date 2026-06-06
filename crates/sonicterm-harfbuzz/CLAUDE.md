# sonicterm-harfbuzz

## Purpose
Harfbuzz binding crate used for shaping and color glyph paint data.

## Public surface
- Low-level Harfbuzz APIs required by `sonicterm-font`.

## Test gate
```bash
cargo test -p sonicterm-harfbuzz --lib
```

## Common pitfalls
- Keep unsafe FFI boundaries explicit.
- Shaping changes can affect ASCII, ligatures, CJK, emoji, and fallback.

## Cross-references
- Consumed by: `sonicterm-font`.
