# sonicterm-gpu

## Purpose
wgpu renderer. It turns `sonicterm-render-model` frames plus shaped text
into GPU draws: quads for chrome/cursor/selection and text batches for
terminal/UI glyphs.

## Key files
- `core.rs` - renderer owner, frame assembly, surface lifecycle.
- `quad.rs` - cursor, selection, underline, pane border, and UI quads.
- `text_pipeline.rs` - glyph instance text draws via the shared atlas.
- `atlas_upload.rs` - glyph atlas uploads.
- `row_quad_cache.rs` - row background/quad caching.
- `chrome_text.rs`, `cursor.rs`, `color.rs` - UI text/cursor/color helpers.

## Local gate
```bash
cargo build -p sonicterm-gpu
```

## Guardrails
- `core.rs` and `text_pipeline.rs` are hot files; keep changes narrow.
- Preserve per-cell foreground/background, inverse, underline, and 256-color
  semantics when moving data through the renderer.
- Drop `wgpu::SurfaceTexture` before reconfiguring the surface after a
  suboptimal frame.
- Upgrade `wgpu` and the `sonicterm-font`/`sonicterm-text` glyph stack as a
  tested set, not one at a time. (`glyphon`/`cosmic-text` were removed; text
  now flows through the Sonic-owned atlas + rasterizer.)

## Cross-references
- Consumes: `sonicterm-render-model`, `sonicterm-text`, `sonicterm-types`,
  `sonicterm-engine`, `sonicterm-block-glyph`. Terminal-grid, config/theme, and
  UI-state types are reached only through `render_model::boundary::{grid, cfg, ui}`
  — the renderer no longer depends on `sonicterm-cfg`/`sonicterm-ui`/`sonicterm-grid`
  directly, so `render-model` is the single declared vt/grid -> gpu and ui -> gpu seam.
- Consumed by: `sonicterm-app`.
