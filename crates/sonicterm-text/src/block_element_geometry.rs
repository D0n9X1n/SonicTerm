//! Per-codepoint sub-cell geometry for Unicode Block Elements (U+2580..=U+259F).
//!
//! Block-element glyphs are not ordinary text — they describe terminal cell
//! geometry directly (full block, halves, eighths, shades, quadrants).
//! Drawing them via `swash`'s natural glyph placement produces visibly
//! squashed output because the font's bbox doesn't always equal the
//! semantically-required sub-cell rect.
//!
//! This module returns the *correct* geometry per codepoint so the renderer
//! can emit one or more cell-aligned quads instead of a font glyph.
//!
//! See `crates/sonicterm-text/src/swash_rasterizer.rs::SymbolFit::BlockCellFill`
//! for the classifier hook; #461 for the visual regression that motivated it.

/// Geometry to emit for a single block-element cell.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum BlockGeometry {
    /// One rect at full foreground color.
    SingleRect(f32, f32, f32, f32),
    /// Two or more rects (multi-quadrant chars U+2599..=U+259F minus singletons).
    MultiRect(Vec<(f32, f32, f32, f32)>),
    /// Full-cell rect at reduced alpha (shaded chars U+2591/U+2592/U+2593).
    ShadedRect((f32, f32, f32, f32), f32),
}

/// Returns the cell-aligned geometry for a Block Elements codepoint, or
/// `None` if `ch` is outside U+2580..=U+259F.
///
/// `cell_origin` is the cell's top-left in logical (or whatever) pixels;
/// `cell_size` is `(width, height)`. Returned rects share the same
/// coordinate system as the inputs.
pub fn block_element_rect(
    ch: char,
    cell_origin: (f32, f32),
    cell_size: (f32, f32),
) -> Option<BlockGeometry> {
    let (x, y) = cell_origin;
    let (w, h) = cell_size;
    let ul = (x, y, w * 0.5, h * 0.5);
    let ur = (x + w * 0.5, y, w * 0.5, h * 0.5);
    let ll = (x, y + h * 0.5, w * 0.5, h * 0.5);
    let lr = (x + w * 0.5, y + h * 0.5, w * 0.5, h * 0.5);
    match ch as u32 {
        // Upper half
        0x2580 => Some(BlockGeometry::SingleRect(x, y, w, h * 0.5)),
        // Lower eighths anchored at cell bottom
        0x2581 => Some(BlockGeometry::SingleRect(x, y + h * 0.875, w, h * 0.125)),
        0x2582 => Some(BlockGeometry::SingleRect(x, y + h * 0.75, w, h * 0.25)),
        0x2583 => Some(BlockGeometry::SingleRect(x, y + h * 0.625, w, h * 0.375)),
        0x2584 => Some(BlockGeometry::SingleRect(x, y + h * 0.5, w, h * 0.5)),
        0x2585 => Some(BlockGeometry::SingleRect(x, y + h * 0.375, w, h * 0.625)),
        0x2586 => Some(BlockGeometry::SingleRect(x, y + h * 0.25, w, h * 0.75)),
        0x2587 => Some(BlockGeometry::SingleRect(x, y + h * 0.125, w, h * 0.875)),
        0x2588 => Some(BlockGeometry::SingleRect(x, y, w, h)),
        // Left eighths anchored at cell left
        0x2589 => Some(BlockGeometry::SingleRect(x, y, w * 0.875, h)),
        0x258A => Some(BlockGeometry::SingleRect(x, y, w * 0.75, h)),
        0x258B => Some(BlockGeometry::SingleRect(x, y, w * 0.625, h)),
        0x258C => Some(BlockGeometry::SingleRect(x, y, w * 0.5, h)),
        0x258D => Some(BlockGeometry::SingleRect(x, y, w * 0.375, h)),
        0x258E => Some(BlockGeometry::SingleRect(x, y, w * 0.25, h)),
        0x258F => Some(BlockGeometry::SingleRect(x, y, w * 0.125, h)),
        // Right half
        0x2590 => Some(BlockGeometry::SingleRect(x + w * 0.5, y, w * 0.5, h)),
        // Shades — full cell with alpha multiplier
        0x2591 => Some(BlockGeometry::ShadedRect((x, y, w, h), 0.25)),
        0x2592 => Some(BlockGeometry::ShadedRect((x, y, w, h), 0.5)),
        0x2593 => Some(BlockGeometry::ShadedRect((x, y, w, h), 0.75)),
        // Upper / right one eighth
        0x2594 => Some(BlockGeometry::SingleRect(x, y, w, h * 0.125)),
        0x2595 => Some(BlockGeometry::SingleRect(x + w * 0.875, y, w * 0.125, h)),
        // Single-quadrant chars
        0x2596 => Some(BlockGeometry::SingleRect(ll.0, ll.1, ll.2, ll.3)),
        0x2597 => Some(BlockGeometry::SingleRect(lr.0, lr.1, lr.2, lr.3)),
        0x2598 => Some(BlockGeometry::SingleRect(ul.0, ul.1, ul.2, ul.3)),
        // Multi-quadrant chars
        0x2599 => Some(BlockGeometry::MultiRect(vec![ul, ll, lr])),
        0x259A => Some(BlockGeometry::MultiRect(vec![ul, lr])),
        0x259B => Some(BlockGeometry::MultiRect(vec![ul, ur, ll])),
        0x259C => Some(BlockGeometry::MultiRect(vec![ul, ur, lr])),
        0x259D => Some(BlockGeometry::SingleRect(ur.0, ur.1, ur.2, ur.3)),
        0x259E => Some(BlockGeometry::MultiRect(vec![ur, ll])),
        0x259F => Some(BlockGeometry::MultiRect(vec![ur, ll, lr])),
        _ => None,
    }
}

/// Returns the dominant single rect for a `BlockGeometry` — useful for
/// callers that need a single bounding rect (e.g. the existing
/// `apply_symbol_fit` signature). For `MultiRect`, returns the first
/// rect; the renderer should detect MultiRect via the full enum and
/// emit additional quads via `block_element_rect` directly.
pub fn primary_rect(geom: &BlockGeometry) -> (f32, f32, f32, f32) {
    match geom {
        BlockGeometry::SingleRect(x, y, w, h) => (*x, *y, *w, *h),
        BlockGeometry::ShadedRect(rect, _) => *rect,
        BlockGeometry::MultiRect(rects) => rects.first().copied().unwrap_or((0.0, 0.0, 0.0, 0.0)),
    }
}
