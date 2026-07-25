# sonicterm-resource

## Purpose
Concrete resource governor, owner registry, accounting ledger, and RAII reservation tokens.

## Guardrails
- Keep reservations coarse; never reserve per cell, parser byte, or rendered glyph.
- Do not add a process-global hot-path mutex.
- Every failure preserves original ownership and accounting.
- All tests use flat sibling `*_tests.rs` files.

## Local gate
```bash
cargo test -p sonicterm-resource --all-features
```
