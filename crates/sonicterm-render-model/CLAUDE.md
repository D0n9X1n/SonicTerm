# sonicterm-render-model

## Purpose
Renderer-agnostic frame model. This crate describes panes, geometry,
input/render data, and painter commands without depending on wgpu or
winit.

## Key files
- `pane_render.rs` - pane frame/model assembly.
- `geometry.rs` - rectangles, sizes, and layout helpers.
- `inputs.rs` - render input structs from app/grid/UI state.
- `painter.rs` - small, currently unimplemented drawing-command abstraction.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-render-model
```

## Guardrails
- Keep renderer-specific GPU choices out of this crate.
- Preserve enough per-cell style data for colors, inverse, underline,
  hyperlinks, cursor, and search highlights.
- Hovered plain-text targets use one canonical fixed-capacity set of at most eight
  ordered, non-empty viewport fragments. Keep it allocation-free and `Copy` so it
  remains safe in retained frame keys and render hot paths.

## Cross-references
- Consumes: `sonicterm-types`, `sonicterm-grid`, `sonicterm-cfg`,
  `sonicterm-ui`.
- Consumed by: `sonicterm-gpu`, `sonicterm-app`.
