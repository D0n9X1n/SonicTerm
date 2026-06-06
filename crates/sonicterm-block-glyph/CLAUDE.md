# sonicterm-block-glyph

## Purpose
Block/box/Powerline/Braille/sextant/octant custom glyph geometry. This crate
contains WezTerm-derived geometry adapted to SonicTerm's renderer.

## Public surface
- Custom glyph classification and geometry generation helpers.

## Test gate
```bash
cargo test -p sonicterm-block-glyph --lib
```

## Common pitfalls
- Preserve `LICENSE-WEZTERM` when touching absorbed WezTerm code.
- Geometry changes are visual changes; smoke-check CJK/emoji/Powerline output.

## Cross-references
- Consumed by: `sonicterm-font`, `sonicterm-gpu`.
