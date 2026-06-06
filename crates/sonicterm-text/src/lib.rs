//! sonicterm-text — headless text layer.
//!
//! Houses everything needed to turn `(Cell, font, size) → rasterized glyph
//! masks in an atlas`: text shaping (sonicterm-font), per-glyph rasterization
//! (sonicterm-font / freetype via `sonicterm-engine::FontStack`), the GPU-shaped atlas
//! + LRU, and the per-row glyph cache.
//!
//! This crate is pure CPU — **no wgpu, no winit**. Downstream `sonicterm-gpu`
//! consumes [`GlyphInstance`] records produced here and uploads them.
//!
//! Imports of the form `sonicterm_text::shape::*`, `sonicterm_text::glyph_atlas::*`,
//! etc. continue to work via `pub use` re-exports in `sonicterm-shared`.

#![forbid(unsafe_op_in_unsafe_fn)]

// T13/T14 (chrome migration) co-landed the T10 hard-delete of the
// cosmic-text + swash modules — the only consumer (sonicterm-gpu)
// stopped referencing every symbol in those files once chrome moved
// to `chrome_text::layout`, so leaving the modules on disk would
// break the workspace gate (the legacy files import deleted
// helpers that already broke during T8/T9). The deleted module
// list mirrors the v5 spec §"What dies":
//
// - `async_fallback.rs`        — cosmic-text-driven fallback loader
// - `block_element_geometry.rs`— SonicTerm-specific Block Element
//                                geometry overlay (wezterm vendored
//                                customglyph in T7 covers the same set)
// - `box_drawing_geometry.rs`  — likewise box-drawing overlay
// - `metrics.rs`               — cosmic-text `measure_cell` etc.
// - `prewarm.rs`               — pre-bake walker (swash-driven)
// - `swash_rasterizer.rs`      — SwashRasterizer + Rasterizer impl
//
// `row_glyph_cache.rs` is rewritten to drop the cosmic-text `Color`
// field type and keeps its data-shape interface; it ships in the
// same edit as a stub the renderer can construct against.
pub mod glyph_atlas;
pub mod row_glyph_cache;
pub mod shape;

/// One drawable glyph in NDC space with its atlas UV rect and color.
///
/// This is the hand-off record between the CPU text layer and the GPU
/// text pass. It lives here (not in `sonicterm-gpu`) because the row-glyph
/// cache pre-builds vectors of these from shaping output, well before
/// any GPU work happens. The struct carries only `[f32; 4]` arrays so
/// it has no wgpu dependency.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphInstance {
    /// `[x, y, w, h]` in NDC (–1..1). `w`/`h` are signed because the
    /// Y axis flips between screen and NDC.
    pub rect: [f32; 4],
    /// `[u0, v0, u1, v1]` normalized atlas coordinates from
    /// `GlyphInfo::uv`.
    pub uv: [f32; 4],
    /// `[r, g, b, a]` foreground color the alpha is modulated by.
    /// For color glyphs (`flags.x >= 0.5`) this is ignored — the
    /// fragment shader returns the premultiplied texture sample
    /// directly so the emoji's own colors come through.
    pub color: [f32; 4],
    /// Per-instance flags packed into a vec4 to keep WGSL vertex
    /// attribute slots simple. `flags.x` is the is-color toggle
    /// (>= 0.5 → color glyph). The remaining components are reserved
    /// for future use (e.g. signed-distance-field weight, oblique
    /// shear) and currently always zero.
    pub flags: [f32; 4],
}
