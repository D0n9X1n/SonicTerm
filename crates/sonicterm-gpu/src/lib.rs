//! sonicterm-gpu — wgpu pipeline primitives behind the renderer-model boundary.
//!
//! This crate owns the wgpu-touching primitives that the terminal renderer
//! composes per frame:
//!
//!   * [`quad`] — the WGSL quad pipeline + `QuadInstance` (cursor, selection,
//!     rounded-rect chrome, panel backgrounds, underlines).
//!   * [`text_pipeline`] — the instanced text pipeline that consumes
//!     `sonicterm_text::GlyphInstance` and samples the GPU glyph atlas.
//!   * [`atlas_upload`] — wgpu-side wrapper around `sonicterm_text::glyph_atlas`
//!     that owns the texture/view/sampler/bind-group and syncs dirty tiles.
//!   * [`chrome_text`] — WezTerm-driven helper that batches chrome strings
//!     (tab titles, palette, search bar, IME, drag chip) into the shared
//!     atlas and text pipeline.
//!
//! The composite renderer (`sonicterm-shared::render`) lives in
//! `sonicterm-shared`, split across sub-files.
//!
//! Dependency rule: `sonicterm-gpu` may depend on `sonicterm-types`, `sonicterm-text`, and
//! `sonicterm-render-model` only. It must NOT depend on `sonicterm-ui` or `sonicterm-shared`
//! — those depend on `sonicterm-gpu`, so a back-edge would create a cycle.

#![deny(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(missing_docs)] // Public contracts are checked separately from private renderer implementation seams.

/// wgpu-side wrapper around `sonicterm_text::glyph_atlas` — owns the texture,
/// view, sampler, and bind group; syncs dirty tiles to the GPU.
pub mod atlas_upload;
/// Wezterm-driven chrome text helper. Replaces
/// the 11 glyphon `TextRenderer` chrome sites and feeds the existing
/// [`text_pipeline`] buffer — no second atlas, no second pass.
pub mod chrome_text;
/// Color / sRGB conversion helpers that produce `wgpu::Color` and linear RGBA
/// arrays from chrome-text colors and `#rrggbb` strings. They consume
/// [`color::ChromeColor`] and keep GPU color conversion behind this crate's
/// renderer-model boundary.
pub mod color;
/// Cursor-related rendering helpers for hollow rectangles, glyph recolouring,
/// and inactive-pane cursor records. All helpers emit `QuadInstance` or
/// `GlyphInstance` data on the GPU side of the renderer-model boundary.
pub mod cursor;
/// Quad pipeline (`QuadInstance` + WGSL): cursor blocks, selection tint,
/// rounded chrome, underlines, focus borders.
pub mod quad;
/// Per-row cache for background, underline, and hyperlink-tint
/// `QuadInstance`s on the GPU side of the renderer-model boundary.
pub mod row_quad_cache;
#[cfg(target_os = "windows")]
pub(crate) mod software_windows;
/// Instanced text pipeline consuming `sonicterm_text::GlyphInstance` and
/// sampling the GPU glyph atlas.
pub mod text_pipeline;
/// WezTerm-style final presentation pipeline. This is the single wgpu draw
/// path for atlas glyphs and colored geometry.
pub mod wezterm_pipeline;

/// Composite terminal renderer (`GpuRenderer`) that combines quad geometry,
/// atlas glyphs, and cursor state in the WezTerm-style presentation pipeline.
pub mod core;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
