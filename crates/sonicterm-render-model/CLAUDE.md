# sonicterm-render-model

## Purpose
Renderer-agnostic frame model. This crate describes panes, geometry,
input/render data, and painter commands without depending on wgpu or
winit.

## Key files
- `pane_render.rs` - pane frame/model assembly.
- `geometry.rs` - rectangles, sizes, and layout helpers.
- `inputs.rs` - render input structs from app/grid/UI state.
- `painter.rs` - painter command bridge.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-render-model
```

## Guardrails
- Keep renderer-specific GPU choices out of this crate.
- Preserve enough per-cell style data for colors, inverse, underline,
  hyperlinks, cursor, and search highlights.

## Cross-references
- Consumes: `sonicterm-types`, `sonicterm-grid`, `sonicterm-ui`.
- Consumed by: `sonicterm-gpu`, `sonicterm-app`.
