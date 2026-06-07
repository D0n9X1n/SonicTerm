# sonicterm-freetype

## Purpose
Generated FreeType FFI bindings plus fixed-point helpers. This crate is
raw ABI surface for glyph loading/rasterization; safe ownership and error
handling live in `sonicterm-font`.

## Key files
- `src/lib.rs` - FreeType bindgen output.
- `src/types.rs` - supplemental FreeType type definitions.
- `src/fixed_point.rs` - fixed-point conversion helpers.

## Local gate
```bash
cargo build -p sonicterm-freetype
```

## Guardrails
- Do not hide unsafe lifetime or ownership rules here; wrap them in
  `sonicterm-font::ftwrap`.
- Avoid style-only churn in generated bindings.
- Keep constants and type widths aligned with the vendored FreeType ABI.

## Cross-references
- Consumed by: `sonicterm-font`.
