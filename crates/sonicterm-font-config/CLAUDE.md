# sonicterm-font-config

## Purpose
Low-level Fontconfig constants and C type definitions used by the Sonic
font locator. This crate is intentionally thin and exists so higher-level
font code does not hand-roll FFI constants.

## Key files
- `src/lib.rs` - Fontconfig constants, enums, and structs.

## Local gate
```bash
cargo build -p sonicterm-font-config
```

Tests: unit tests live in `tests/<module>_tests.rs` (`#[path]`-included from `src`); see the root `CLAUDE.md` Conventions.

## Guardrails
- Treat this as binding/support code; avoid broad rewrites for style only.
- Keep numeric constants aligned with Fontconfig.
- Higher-level matching logic belongs in `sonicterm-font`, not here.

## Cross-references
- Consumed by: `sonicterm-font`.
