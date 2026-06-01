# sonicterm-render-model

## Purpose
Renderer-agnostic frame model. Describes *what* to draw — geometry,
inputs, painter trait — not *how*. Sits between `sonicterm-grid` /
`sonicterm-ui` and `sonicterm-gpu`. Pure data.

## Public surface
- `geometry`, `inputs`
- `painter` traits — re-export point for `sonicterm-types::Painter`

## Land-mines specific to this crate
None named in §4; but this crate is the dividing line that lets the GPU
backend be swapped. Don't add winit or wgpu types here.

## Test gate (local)
```bash
cargo test -p sonicterm-render-model
```

## Common pitfalls
- Importing `winit` or `wgpu` — kills the swappability promise
- Storing GPU resource handles in the model — they belong in `sonicterm-gpu`

## Owning PM(s)
- Primary: either (pure-data)
- Hot-file: no (additive surface mostly)

## Cross-references
- Consumes traits from: `sonicterm-types`
- Consumed by: `sonicterm-gpu`, `sonicterm-app`
