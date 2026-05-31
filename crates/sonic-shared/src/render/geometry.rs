//! Geometry helpers for the renderer.
//!
//! Currently hosts the device-pixel snap used by the per-cell glyph
//! emission paths in `core.rs` (issue #405). Kept as a small free
//! function so it can be unit-tested without a wgpu surface.

/// Snap a logical-space `(x, y, w, h)` rect so that its four edges
/// land exactly on device pixels (i.e. `edge * scale` is an integer).
///
/// # Rationale
///
/// On Windows at fractional DPI (the common 125 % / 150 % laptop
/// defaults), the cell grid is laid out in logical units derived from
/// the rasterized font metrics (`cell_w` ≈ 8.4 logical px, for
/// example). When those logical edges are mapped to NDC and then to
/// the framebuffer, the glyph quad straddles physical pixel borders
/// and the GPU's bilinear sample of the atlas tile produces a visibly
/// blurred glyph. Snapping the quad edges to integer device-pixel
/// positions before NDC conversion realigns the sample grid with the
/// atlas grid and restores per-pixel sharpness.
///
/// # Integer-scale fast path
///
/// At `scale == 1.0` (Windows 100 %) and `scale == 2.0` (Mac Retina),
/// the existing layout already produces device-aligned edges in
/// practice, and rounding a font-derived `cell_w` like 8.4 would shift
/// it by ~0.5 logical px — enough to force a visual-snapshot baseline
/// bump on Mac (see CLAUDE.md §11 "Render hot-file rule"). We therefore
/// short-circuit when `scale.fract() == 0.0`. This keeps the Mac
/// dHash gate green without touching baselines and confines the
/// behavior change to the fractional-DPI machines that actually need
/// it (Windows 125 %, 150 %, 175 %, …).
///
/// # Edge-based, not width-independent
///
/// We snap the LEFT/TOP/RIGHT/BOTTOM device-pixel coordinates and
/// derive width and height as their differences. Snapping `w` and `h`
/// independently of `x`/`y` would accumulate up-to-±0.5-device-pixel
/// drift across a row, leaving visible gaps or overlaps between
/// adjacent glyph quads.
pub fn snap_to_device_pixels(rect: (f32, f32, f32, f32), scale: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = rect;
    // Integer-scale fast path — see module doc.
    if scale.fract() == 0.0 {
        return rect;
    }
    let x_dev = (x * scale).round();
    let y_dev = (y * scale).round();
    let r_dev = ((x + w) * scale).round();
    let b_dev = ((y + h) * scale).round();
    let inv = 1.0 / scale;
    (
        x_dev * inv,
        y_dev * inv,
        (r_dev - x_dev) * inv,
        (b_dev - y_dev) * inv,
    )
}
