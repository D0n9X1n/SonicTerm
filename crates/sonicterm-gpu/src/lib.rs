//! sonicterm-gpu — GPU pipeline primitives split out of `sonicterm-shared` in PR 7a
//! of the workspace refactor (see `docs/specs/2026-04-22-workspace-refactor.md`).
//!
//! This crate owns the wgpu/glyphon/cosmic-text-touching primitives that the
//! terminal renderer composes per frame:
//!
//!   * [`quad`] — the WGSL quad pipeline + `QuadInstance` (cursor, selection,
//!     rounded-rect chrome, panel backgrounds, underlines).
//!   * [`text_pipeline`] — the instanced text pipeline that consumes
//!     `sonicterm_text::GlyphInstance` and samples the GPU glyph atlas.
//!   * [`atlas_upload`] — wgpu-side wrapper around `sonicterm_text::glyph_atlas`
//!     that owns the texture/view/sampler/bind-group and syncs dirty tiles.
//!
//! The composite renderer (`sonicterm-shared::render`) still lives in
//! `sonicterm-shared`; PR 7b split `render.rs` into sub-files.
//!
//! Dependency rule: `sonicterm-gpu` may depend on `sonicterm-types`, `sonicterm-text`, and
//! `sonicterm-render-model` only. It must NOT depend on `sonicterm-ui` or `sonicterm-shared`
//! — those depend on `sonicterm-gpu`, so a back-edge would create a cycle.

#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(missing_docs)] // core.rs (moved in M7f) carries its own doc coverage; relax until follow-up.

/// wgpu-side wrapper around `sonicterm_text::glyph_atlas` — owns the texture,
/// view, sampler, and bind group; syncs dirty tiles to the GPU.
pub mod atlas_upload;
/// Color / sRGB conversion helpers — produce `wgpu::Color` / linear RGBA arrays
/// from glyphon colors and `#rrggbb` hex strings. Moved here from
/// `sonicterm-shared::render::color` in M7b of the workspace refactor; the
/// helpers consume `glyphon::Color` and produce `wgpu::Color`, so they belong
/// on the GPU side of the layer split.
pub mod color;
/// Cursor-related rendering helpers (hollow rects, glyph recolouring,
/// inactive-pane cursor record). Moved from
/// `sonicterm-shared::render::cursor` in M7e of the workspace refactor —
/// all helpers emit `QuadInstance` / `GlyphInstance` and belong on the
/// GPU side of the layer split.
pub mod cursor;
/// Per-codepoint geometry → `QuadInstance` bridge for #542. Single
/// funnel used by all three GPU emit branches in `core.rs` so the
/// "fix only one branch" anti-pattern stays fixed.
pub mod geometry_emit;
/// Quad pipeline (`QuadInstance` + WGSL): cursor blocks, selection tint,
/// rounded chrome, underlines, focus borders.
pub mod quad;
/// Per-row quad cache for background / underline / hyperlink tint quads.
/// Moved from `sonicterm-shared::render::row_quad_cache` in M7e — caches
/// `QuadInstance`, so it belongs on the GPU side of the layer split.
pub mod row_quad_cache;
/// Instanced text pipeline consuming `sonicterm_text::GlyphInstance` and
/// sampling the GPU glyph atlas.
pub mod text_pipeline;

/// Composite terminal renderer (`GpuRenderer`). Moved here from
/// `sonicterm-shared::render::core` in M7f of the workspace refactor —
/// the renderer is the GPU pipeline that composes `quad`, `text_pipeline`,
/// `atlas_upload`, and `cursor` into per-frame draws, so it belongs on
/// the GPU side. `sonicterm-shared::render` is now a thin deprecated
/// re-export shim around this module.
pub mod core;
