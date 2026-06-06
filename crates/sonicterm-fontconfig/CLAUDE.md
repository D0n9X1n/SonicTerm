# sonicterm-fontconfig

## Purpose
Unix fontconfig build/link shim used by SonicTerm's font stack where available.

## Public surface
- Build-time linkage detection and minimal wrapper surface.

## Test gate
```bash
cargo test -p sonicterm-fontconfig --lib
```

## Common pitfalls
- Keep macOS/Windows builds working when fontconfig is absent.
- Do not make runtime fontconfig mandatory for bundled-font paths.

## Cross-references
- Consumed by: `sonicterm-font`.
