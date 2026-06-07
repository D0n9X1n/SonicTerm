# sonicterm-font

## Purpose
Sonic-owned font stack: font database, platform locators, parsing,
HarfBuzz shaping, FreeType rasterization, color glyph handling, and cache
keys. New font behavior should land here instead of depending on
`vendor/` or WezTerm font paths.

## Key files
- `db.rs` - font database and family/style lookup.
- `locator/` - CoreText, GDI, and Fontconfig locators.
- `shaper/harfbuzz.rs` - shaping boundary.
- `rasterizer/` - FreeType, HarfBuzz, and COLR raster support.
- `parser.rs`, `rangeset.rs`, `units.rs`, `color.rs` - shared font helpers.
- `fcwrap.rs`, `ftwrap.rs`, `hbwrap.rs` - FFI-safe wrappers.

## Local gate
```bash
cargo build -p sonicterm-font
```

## Guardrails
- Cache keys must include family, size, weight, style, stretch, and glyph
  variation data that affects output.
- Keep platform discovery behind `locator/`; callers should not branch on
  CoreText/GDI/Fontconfig details.
- Do not introduce direct dependencies on vendor font modules.

## Cross-references
- Consumes: `sonicterm-font-config`, `sonicterm-fontconfig`,
  `sonicterm-freetype`, `sonicterm-harfbuzz`.
- Consumed by: `sonicterm-text`, renderer/font integration code.
