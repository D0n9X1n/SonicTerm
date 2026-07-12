# sonicterm-harfbuzz

## Purpose
Generated HarfBuzz FFI bindings. This crate is raw ABI surface for
shaping; safe buffer/font wrappers and shaping policy live in
`sonicterm-font`.

## Key files
- `src/lib.rs` - bindgen output for HarfBuzz.

## Local gate
```bash
cargo build -p sonicterm-harfbuzz
```

Regenerate the bindings from the repository root with
`bash scripts/regenerate-harfbuzz.sh`, then review the generated diff.

## Guardrails
- Avoid manual edits to generated bindings except targeted compatibility
  fixes.
- Safe lifetime management belongs in `sonicterm-font::hbwrap`.
- Keep allow attributes local to this crate.

## Cross-references
- Consumed by: `sonicterm-font`.
