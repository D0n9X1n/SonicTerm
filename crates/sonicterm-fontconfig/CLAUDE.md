# sonicterm-fontconfig

## Purpose
Hand-written Fontconfig FFI declarations. This crate is the raw syscall/ABI
surface; ergonomic matching and fallback policy belong in
`sonicterm-font`.

## Key files
- `src/lib.rs` - hand-written Fontconfig FFI types, constants, and extern
  declarations; no generator produces it.

## Local gate
```bash
cargo build -p sonicterm-fontconfig
```

## Guardrails
- Keep `src/lib.rs` a faithful mirror of the Fontconfig C ABI; change it only
  for a targeted compatibility patch.
- Keep allow attributes local to this binding crate.
- Safe wrappers belong in `sonicterm-font::fcwrap`.

## Cross-references
- Consumed by: `sonicterm-font`.
