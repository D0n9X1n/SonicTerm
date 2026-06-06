# sonicterm-font

## Purpose
WezTerm-compatible font discovery, fallback, shaping, and rasterization.
SonicTerm should depend on this crate instead of vendor font paths.

## Public surface
- Font stack construction.
- Shaping/rasterization helpers consumed by the GPU and text crates.

## Test gate
```bash
cargo test -p sonicterm-font --lib
```

## Common pitfalls
- Font fallback order affects every visible glyph.
- Keep CJK, emoji, Nerd Font PUA, box drawing, and Powerline behavior intact.
- Avoid broad `as any`-style type erasure; model font attributes precisely.

## Cross-references
- Consumes: `sonicterm-freetype`, `sonicterm-harfbuzz`,
  `sonicterm-block-glyph`.
- Consumed by: `sonicterm-text`, `sonicterm-gpu`.
