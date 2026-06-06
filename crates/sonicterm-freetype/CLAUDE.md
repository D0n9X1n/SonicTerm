# sonicterm-freetype

## Purpose
Freetype/libpng/zlib binding crate used by the SonicTerm font rasterizer.

## Public surface
- Low-level generated/bound Freetype APIs.

## Test gate
```bash
cargo test -p sonicterm-freetype --lib
```

## Common pitfalls
- Treat generated bindings as low-level unsafe surface.
- Keep bundled C dependency build flags portable across macOS and Windows.

## Cross-references
- Consumed by: `sonicterm-font`.
