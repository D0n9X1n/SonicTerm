# sonicterm-font-config

## Purpose
Pure font configuration values and parsing helpers shared by font discovery and
rendering code.

## Public surface
- Font family/style/weight/stretch configuration types.

## Test gate
```bash
cargo test -p sonicterm-font-config --lib
```

## Common pitfalls
- Keep this crate pure-data; no platform APIs here.
- TOML-facing names are user-visible and need migration care.

## Cross-references
- Consumed by: `sonicterm-font`.
