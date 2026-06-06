# sonicterm-engine

## Purpose
Font-engine adapter surface used by the renderer after the WezTerm font behavior
absorption. Owns the stable interface between high-level rendering and
`sonicterm-font`.

## Public surface
- Font stack/types re-exported to consumers that should not depend on lower font
  implementation details.

## Test gate
```bash
cargo test -p sonicterm-engine --lib
```

## Common pitfalls
- Do not add winit/wgpu dependencies here.
- Keep the API small; broad font behavior belongs in `sonicterm-font`.

## Cross-references
- Consumed by: `sonicterm-render-model`, `sonicterm-gpu`.
