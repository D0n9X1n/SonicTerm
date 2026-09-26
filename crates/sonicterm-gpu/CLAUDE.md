# sonicterm-gpu

## Purpose
wgpu renderer. It turns `sonicterm-render-model` frames plus shaped text
into GPU draws: quads for chrome/cursor/selection and text batches for
terminal/UI glyphs.

## Key files
- `core.rs` - renderer owner, frame assembly, surface lifecycle.
- `device_errors.rs` - per-device wgpu error and loss state, the GPU-work gate,
  the frame-outcome decision, and the test fault kinds.
- `frame_plan.rs` - owned metadata-only key, mode, damage, clips, viewport slots, and revision expectations.
- `present.rs` - the presentation seam: the wgpu and Windows GDI presenters and the typed `PresentOutcome`.
- `software_frame.rs` - platform-neutral CPU composition and flat sibling pixel tests.
- `software_windows.rs` - Windows-only HWND/HDC presentation of a validated borrowed frame.
- `quad.rs` - cursor, selection, underline, pane border, and UI quads.
- `wezterm_pipeline.rs` - production glyph and geometry presentation via the shared atlas.
- `text_pipeline.rs` - legacy alpha-only compatibility pipeline.
- `atlas_upload.rs` - glyph atlas uploads.
- `row_quad_cache.rs` - row background/quad caching.
- `chrome_text.rs`, `cursor.rs`, `color.rs` - UI text/cursor/color helpers.

## Local gate
```bash
cargo build -p sonicterm-gpu
```

## Guardrails
- `core.rs` and `text_pipeline.rs` are hot files; keep changes narrow.
- CPU composition stays in `software_frame`, compiled for Windows and every host's
  unit tests, with no native imports or unsafe code. Preserve pixel assertions and
  tolerances, including the headless wgpu comparisons. Only the Windows bridge
  receives a borrowed `BgraFrame`; non-Windows production has no CPU presenter.
- Production consumes `FramePlan` decisions once; keep grids, UI controllers, native handles, and copied rows out of the plan. Parser guards still span presentation.
- A presented plan acknowledges only matching pane ids and grid revisions; retry and failure paths retain dirt.
- Only `PresentOutcome::Presented` acknowledges a plan. A stopped device is always
  `RenderingUnavailable`, never a surface retry or a presented frame; `render`
  keeps its `Result<()>` by mapping the outcome.
- Preserve per-cell foreground/background, inverse, underline, and 256-color
  semantics when moving data through the renderer.
- Row glyph cache reads and writes use the atlas content identity; eviction
  counts remain diagnostic and must not become UV-bearing cache keys.
- Multi-row hover fragments share one frame-key identity and one underline pass.
  Active recolor salts only the intersecting row cache key; hint-only fragments
  reuse ordinary glyph rows, and offscreen or out-of-column spans emit nothing.
- Drop `wgpu::SurfaceTexture` before reconfiguring the surface after a
  suboptimal frame.
- The renderer's retained figures are reported per window and summed across
  the process. A per-window buffer that is never released shows as a
  staircase across window open/close, which is what the churn baseline
  measures; keep new renderer-owned allocations reported through
  `retained_amounts` so they stay visible there.
- `retained_amounts()` answers about the instance you ask, and every instance
  reports the same atlas capacity. It cannot tell you whether the *previous*
  renderer was released — comparing it across open/close cycles compares a
  constant to itself. `live_renderer_count()` is the reading a leak moves; it
  is what the churn baseline asserts on, and its increment in `new` must stay
  paired with the decrement in `Drop`.
- Upgrade `wgpu` and the `sonicterm-font`/`sonicterm-text` glyph stack as a
  tested set, not one at a time. (`glyphon`/`cosmic-text` were removed; text
  now flows through the Sonic-owned atlas + rasterizer.)
- Production instance creation honors `WGPU_BACKEND`; Linux package smokes force
  Vulkan/lavapipe and require a native presentation after the PTY marker arrives.
- Retain packaged font directories across live font reloads; dropping them can
  make a fresh Linux install resolve a different or missing face.
- GPU work runs only through the device gate, while the device is `Usable`.
  Production code pushes no error scopes, never polls the device or instance,
  and uses no render bundles or `wgpu::util` buffer-init helpers: wgpu treats
  poll and bundle errors as fatal, and `create_buffer_init` panics on an
  invalid buffer. Only the test fault hook scopes or polls.

## Cross-references
- Consumes: `sonicterm-render-model`, `sonicterm-text`, `sonicterm-types`,
  `sonicterm-engine`, `sonicterm-block-glyph`. Terminal-grid, config/theme, and
  UI-state types are reached only through `render_model::boundary::{grid, cfg, ui}`
  — the renderer no longer depends on `sonicterm-cfg`/`sonicterm-ui`/`sonicterm-grid`
  directly, so `render-model` is the single declared vt/grid -> gpu and ui -> gpu seam.
- Consumed by: `sonicterm-app`.
