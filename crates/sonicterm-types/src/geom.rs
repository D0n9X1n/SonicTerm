//! Grid geometry value types (positions). `Rect` intentionally lives in
//! per-crate modules because each consumer uses different units (pixels vs
//! cells vs config-space) — there is no canonical workspace `Rect`.

/// (row, col) position. (0, 0) is top-left of the visible region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Pos {
    /// Row index (0 is the top of the visible region).
    pub row: u16,
    /// Column index (0 is the left edge of the visible region).
    pub col: u16,
}

/// Renderer-agnostic geometry quad value type — the canonical shape carried
/// across the crate boundary for cursor / selection / chrome quads. The GPU
/// crate adapts this into its own `QuadInstance` (which additionally
/// derives `bytemuck::Pod` + `Zeroable` for `wgpu`) via
/// `impl From<GeometryQuad> for QuadInstance`.
///
/// Layout intentionally mirrors `sonicterm-gpu::quad::QuadInstance` field
/// for field so the adapter is a lossless 1:1 copy. `#[repr(C)]` keeps the
/// memory layout stable for a future zero-copy path, but **no**
/// `bytemuck::Pod` / `serde` derives live here — the types crate stays
/// dependency-free (per `docs/CONTRACTS.md`). Consumers that need a Pod
/// view should convert into the GPU crate's `QuadInstance`.
///
/// Field semantics match `QuadInstance`:
/// - `rect`: `[x, y, w, h]` in NDC `[-1, 1]`.
/// - `color`: premultiplied-alpha RGBA in linear space.
/// - `size_px`: rect width / height in physical pixels (SDF input).
/// - `radius_px`: rounded-rect corner radius in physical pixels. `0.0`
///   selects the sharp-rect path.
/// - `line_thickness_px`: line-segment stroke thickness in physical
///   pixels. `> 0` selects the capsule-SDF path.
/// - `line_a`, `line_b`: line-segment endpoints in pixel offsets from
///   the rect center. Only consulted when `line_thickness_px > 0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryQuad {
    /// Rectangle as `[x, y, w, h]` in NDC.
    pub rect: [f32; 4],
    /// Premultiplied-alpha RGBA fill color in linear space.
    pub color: [f32; 4],
    /// Rectangle width / height in physical pixels.
    pub size_px: [f32; 2],
    /// Corner radius in physical pixels. `0.0` skips the SDF path.
    pub radius_px: f32,
    /// Line-segment stroke thickness in physical pixels.
    pub line_thickness_px: f32,
    /// Line segment endpoint A, pixel offset from the rect center.
    pub line_a: [f32; 2],
    /// Line segment endpoint B, pixel offset from the rect center.
    pub line_b: [f32; 2],
}
