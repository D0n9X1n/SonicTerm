# sonicterm-render-model

## Purpose
Renderer-agnostic frame model without wgpu or winit dependencies. Production
passes borrowed `PaneRender` inputs and explicit UI state to `GpuRenderer`.
`RenderInputs` and the dormant `Painter` trait remain public compatibility
surfaces; `boundary` is the active grid/config/UI dependency seam.

## Key files
- `pane_render.rs` - pane frame/model assembly.
- `geometry.rs` - rectangles, sizes, and layout helpers.
- `inputs.rs` - render input structs from app/grid/UI state.
- `painter.rs` - legacy, unimplemented drawing-command compatibility trait.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-render-model
```

## Guardrails
- Keep renderer-specific GPU choices out of this crate.
- Preserve enough per-cell style data for colors, inverse, underline,
  hyperlinks, cursor, and search highlights.
- Hovered plain-text targets use one canonical fixed-capacity set of at most 32 (`MAX_HOVERED_URL_SPANS`)
  ordered, non-empty viewport fragments. Keep it allocation-free and `Copy` so it
  remains safe in retained frame keys and render hot paths.

## Cross-references
- Consumes: `sonicterm-types`, `sonicterm-grid`, `sonicterm-cfg`,
  `sonicterm-ui`.
- Consumed by: `sonicterm-gpu`, `sonicterm-app`.
