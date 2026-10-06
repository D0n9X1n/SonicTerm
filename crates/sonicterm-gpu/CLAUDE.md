# sonicterm-gpu

## Purpose
`core::GpuRenderer` assembles borrowed `PaneRender` inputs and explicit UI state
into quads and glyph batches. Its private presenter dispatches wgpu or Windows
GDI presentation. `TextPipeline` remains a callable compatibility pipeline, not
the production glyph path.

## Key files
- `core.rs` - renderer owner, frame assembly, surface lifecycle.
- `atlas_lifecycle.rs` - private atlas reset, promotion/demotion, gated upload rebuild and retry settlement.
- `device_errors.rs` - per-device wgpu error and loss state, the GPU-work gate,
  the frame-outcome decision, and the test fault kinds.
- `frame_plan.rs` - owned metadata-only key, mode, damage, clips, viewport slots, and revision expectations; damage is classified per changed identity field; `Partial` mode and per-pane emitted rows.
- `completeness.rs` - the perf-end glyph completeness certificate: stored at each presented `Full` frame with its scene (`FrameKey::scene`, the key without dirt fields) and atlas stamp, extended by same-scene frames (so the counts are an upper bound after partial updates), read by `GpuRenderer::completeness_checkpoint` only while scene and atlas still match.
- `row_ink.rs` - per-row ink records of the presented frame, valid only for the absolute row and content stamp they were drawn from; a partial plan emits by them.
- `present.rs` - the presentation seam: the wgpu and Windows GDI presenters and the typed `PresentOutcome`.
- `software_frame.rs` - platform-neutral CPU composition and flat sibling pixel tests.
- `software_windows.rs` - Windows-only HWND/HDC presentation of a validated borrowed frame.
- `recovery.rs`, `recovery_context.rs`, `rebind.rs` - pure recovery coordinator, owned context negotiation, and same-callback renderer rebind driven by the App.
- `quad.rs` - cursor, selection, underline, pane border, and UI quads.
- `wezterm_pipeline.rs` - production glyph and geometry presentation via the shared atlas.
- `text_pipeline.rs` - legacy alpha-only compatibility pipeline.
- `atlas_upload.rs` - glyph atlas uploads.
- `row_quad_cache.rs` - row background/quad caching.
- `row_runs.rs` - a row's visible cells cut into bold/italic style runs as borrowed slices, and the one text-and-column materializer the emitter calls only for a run it shapes; the doc-hidden `__row_shape_runs` test inspector reuses both.
- `chrome_text.rs`, `cursor.rs`, `color.rs` - UI text/cursor/color helpers; prepared chrome runs share shaping between field geometry and glyph emission.
- `field_geometry.rs` - clipped query caret/selection geometry and hit testing bound to the last presented field.
- `tab_title_font.rs` - device-free tab-title font state (stack, raster size, width key) that `set_font`, the scale rebuild and `measure_tab_widths` share.
- `frame_scratch.rs` - renderer-owned per-frame draw vectors; a pass leases them after its unchanged and no-op exits, the lease or presentation restores them on every exit, and every restoration clears each vector and holds it within its cap; only a completed assembly also shrinks by the vertex-scratch rule and drops column-edge slots above its peak, so a failed or retried frame keeps its warm capacity.
- `chrome_cache.rs` - device-free tab-title (64 slots by position), search-overlay run (32 slots) and UI palette caches; `chrome_cache_seam.rs` is the hidden integration-test seam over them.

## Local gate
```bash
cargo build -p sonicterm-gpu
```

## Guardrails
- `trim_for_occlusion` releases only what the next frame rebuilds, inside the device gate; it never touches the glyph atlas, its retry or eviction state. A trimmed frame texture is restored by `ensure_frame_texture`, the first step of the wgpu present, and every other texture install clears the mark.
- `core.rs` and `text_pipeline.rs` are hot files; keep changes narrow.
- CPU composition stays in `software_frame`, compiled for Windows and every host's
  unit tests, with no native imports or unsafe code. Preserve pixel assertions and
  tolerances, including the headless wgpu comparisons. Only the Windows bridge
  receives a borrowed `BgraFrame`; non-Windows production has no CPU presenter.
- Production consumes `FramePlan` decisions once; keep grids, UI controllers, native handles, and copied rows out of the plan.
- Assembly, the release of the frame source and presentation happen in one `render_releasing` call; no assembled frame leaves the renderer, and assembly never reaches the device or a presenter.
- The renderer clears no grid dirt. A presented frame issues metadata `AckReceipt`s; production applies them at the window's next collection, and the `render_with_outcome` compatibility wrapper applies them through its borrowed grids. Retry and failure paths issue none.
- Only `PresentOutcome::Presented` acknowledges a plan. A stopped device is always
  `RenderingUnavailable`, never a surface retry or a presented frame; `render`
  keeps its `Result<()>` by mapping the outcome.
- Preserve per-cell foreground/background, inverse, underline, and 256-color
  semantics when moving data through the renderer.
- Row glyph cache reads and writes use the atlas content identity; eviction
  counts remain diagnostic and must not become UV-bearing cache keys. Settling an
  atlas retry keeps the rows it admitted and drops only the preedit cache.
- The row glyph cache is keyed by content, never by absolute row, slot, origin,
  surface or selection. Hit and miss both project position-free records through
  `project_cached_row`, in the order cell origin, raster offset, shaping offset,
  marker fit, snap, NDC. Admit only complete rows: an atlas refusal (read before
  `drawable_or_tofu`), an empty block, a failed shaping run or a missing shaper
  or rasterizer keeps the row out. Software block rows are revalidated per
  position. Call `begin_frame` once per assembly pass, pin every emitted key with
  the committed slots before the first admission, stage slot keys, and commit
  them only through `settle_retained_frame` on `Presented`; every `Err` exit
  discards the stage. Nothing drops a glyph row for dirt; only the quad cache does.
- Only the glyph atlas is built growable (`start_dim`, then doubling to 2048);
  the promoted image atlas stays fixed. A growth-only stamp change retries
  through `retry_after_glyph_atlas_growth`, reached only from the retry arm
  after `lend`, and never resets the atlas or disables eviction.
- Multi-row hover fragments share one frame-key identity and one underline pass.
  Active recolor salts only the intersecting row cache key; hint-only fragments
  reuse ordinary glyph rows, and offscreen or out-of-column spans emit nothing.
- Drop `wgpu::SurfaceTexture` before reconfiguring the surface after a
  suboptimal frame.
- The renderer's retained figures are reported per window and summed across
  the process. A per-window buffer that is never released shows as a
  staircase across window open/close, which is what the churn baseline
  measures; keep new renderer-owned allocations reported through
  `retained_amounts` so they stay visible there. The frame scratch and the chrome
  caches report as the `frame_scratch` and `chrome_cache` parts.
- A face replacement that can keep a title key (`adopt_font_stacks`,
  `rebuild_for_sf`, `clear_shape_cache`) clears both chrome run caches beside the
  row-cache invalidation; an applied fallback generation empties the chrome-run
  cache through `FontApplyTargets.chrome_runs`, and titles miss by their epoch.
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
