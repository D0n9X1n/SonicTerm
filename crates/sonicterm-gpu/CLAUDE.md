# sonicterm-gpu

## Purpose
wgpu renderer. It turns `sonicterm-render-model` frames plus shaped text
into GPU draws: quads for chrome/cursor/selection and text batches for
terminal/UI glyphs.

## Key files
- `core.rs` - renderer owner, frame assembly, surface lifecycle.
- `quad.rs` - cursor, selection, underline, pane border, and UI quads.
- `text_pipeline.rs` - glyphon/cosmic-text text draws.
- `atlas_upload.rs` - glyph atlas uploads.
- `row_quad_cache.rs` - row background/quad caching.
- `chrome_text.rs`, `cursor.rs`, `color.rs` - UI text/cursor/color helpers.

## Local gate
```bash
cargo build -p sonicterm-gpu
```

Render-touching PRs also need GUI smoke on the originating platform.

## Guardrails
- `core.rs` and `text_pipeline.rs` are hot files; keep changes narrow.
- Preserve per-cell foreground/background, inverse, underline, and 256-color
  semantics when moving data through the renderer.
- Drop `wgpu::SurfaceTexture` before reconfiguring the surface after a
  suboptimal frame.
- Upgrade `wgpu`, `glyphon`, and `cosmic-text` as a tested set, not one at
  a time.

## Cross-references
- Consumes: `sonicterm-render-model`, `sonicterm-text`,
  `sonicterm-types`, `sonicterm-cfg`, `sonicterm-ui`.
- Consumed by: `sonicterm-app`.
