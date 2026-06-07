# sonicterm-fontconfig

## Purpose
Generated Fontconfig FFI bindings. This crate is the raw syscall/ABI
surface; ergonomic matching and fallback policy belong in
`sonicterm-font`.

## Key files
- `src/lib.rs` - bindgen output for Fontconfig.

## Local gate
```bash
cargo build -p sonicterm-fontconfig
```

## Guardrails
- Do not edit generated bindings by hand unless the change is a targeted
  compatibility patch.
- Keep allow attributes local to this binding crate.
- Safe wrappers belong in `sonicterm-font::fcwrap`.

## Cross-references
- Consumed by: `sonicterm-font`.
