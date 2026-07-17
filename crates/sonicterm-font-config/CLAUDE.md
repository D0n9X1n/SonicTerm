# sonicterm-font-config

## Purpose
Font configuration value model used by the Sonic font stack: text styles,
font attributes, weight/stretch/style values, rasterizer selection, and font
policy. The separate `sonicterm-fontconfig` crate owns generated Fontconfig FFI.

## Key files
- `src/lib.rs` - Font configuration structs, enums, defaults, and helpers.

## Local gate
```bash
cargo build -p sonicterm-font-config
```

## Guardrails
- Keep this crate free of font discovery, shaping, and native FFI ownership.
- Preserve serialization/default compatibility for the absorbed font stack.
- Higher-level matching logic belongs in `sonicterm-font`, not here.

## Cross-references
- Consumed by: `sonicterm-font`.
