# sonicterm-gpu

## Purpose
wgpu pipelines. Two pipelines per frame: `quad` (cursor / selection /
underline / hyperlink tint / tab-bar chrome / focused-pane border) and
`text_pipeline` (text via glyphon + cosmic-text). `atlas_upload` moves
swash-rasterized glyphs into texture pages.

## Public surface
- `quad::QuadPipeline`
- `text_pipeline`
- `atlas_upload`

## Land-mines specific to this crate
Render hot-file rule (closes #283): `text_pipeline.rs` and `core.rs`
are visual-sensitive. Keep the app/mac build green and smoke-check
visually.

§4 land-mines that touch this crate:
- **Per-cell ANSI bg** must be emitted (P0, #161 → #163). Don't drop
  the `bg` field on the way to the presentation pipeline.
- **`wgpu::CurrentSurfaceTexture::Suboptimal(frame)` must drop the
  SurfaceTexture before `surface.configure(...)`** (wgpu 29 panic).
- **`wgpu`/`glyphon`/`cosmic-text` are a coherent triple** —
  current: wgpu 29 + glyphon 0.11 + cosmic-text 0.18. Don't upgrade
  just one.

## Test gate (local)
```bash
cargo build -p sonicterm-gpu
```

PR #459: adapter selection in `src/core.rs` emits a WARN when wgpu
falls back to the GLES backend (helps diagnose missing Vulkan/Metal).

## Common pitfalls
- Using `set_text` instead of `set_rich_text` — per-cell color/weight
  collapse into the default attrs
- Forgetting srgb→linear gamma on hex theme values
- Atlas page allocation in the hot path — must be eviction-aware

## Owning PM(s)
- Primary: either; §13 smoke required from BOTH PMs
- Hot-file: yes — render-touching, snapshot-gated

## Cross-references
- Consumes: `sonicterm-text`, `sonicterm-render-model`, `sonicterm-types::Painter`
- Consumed by: `sonicterm-app` directly; legacy `sonicterm-shared::render` shim re-exports `sonicterm_gpu::core` for back-compat (will be removed in v1.1).
