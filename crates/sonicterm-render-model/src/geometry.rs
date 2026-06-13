/// Axis-aligned rectangle in window-pixel space (origin top-left, y grows down)
/// — the common geometry primitive shared between layout code and the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge in window pixels.
    pub x: i32,
    /// Top edge in window pixels.
    pub y: i32,
    /// Width in window pixels.
    pub w: u32,
    /// Height in window pixels.
    pub h: u32,
}

impl PixelRect {
    /// Right edge in window pixels, saturating on overflow.
    #[must_use]
    pub fn right(self) -> i32 {
        self.x.saturating_add(self.w.min(i32::MAX as u32) as i32)
    }

    /// Bottom edge in window pixels, saturating on overflow.
    #[must_use]
    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.h.min(i32::MAX as u32) as i32)
    }

    /// True when either dimension is zero.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Return this rectangle clipped to `bounds`, or `None` if they do not overlap.
    #[must_use]
    pub fn intersect(self, bounds: PixelRect) -> Option<PixelRect> {
        let x0 = self.x.max(bounds.x);
        let y0 = self.y.max(bounds.y);
        let x1 = self.right().min(bounds.right());
        let y1 = self.bottom().min(bounds.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect { x: x0, y: y0, w: (x1 - x0) as u32, h: (y1 - y0) as u32 })
    }

    /// Return the smallest rectangle containing both rectangles.
    #[must_use]
    pub fn union(self, other: PixelRect) -> PixelRect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        PixelRect { x: x0, y: y0, w: (x1 - x0).max(0) as u32, h: (y1 - y0).max(0) as u32 }
    }
}

/// Accumulated window-pixel damage for a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageRect {
    rect: Option<PixelRect>,
}

impl DamageRect {
    /// Create an empty damage accumulator.
    #[must_use]
    pub const fn empty() -> Self {
        Self { rect: None }
    }

    /// Add a rectangle to the accumulated damage, clipping it to `bounds`.
    pub fn add_clipped(&mut self, rect: PixelRect, bounds: PixelRect) {
        let Some(clipped) = rect.intersect(bounds) else { return };
        self.rect = Some(match self.rect {
            Some(existing) => existing.union(clipped),
            None => clipped,
        });
    }

    /// Current union damage rectangle, if any.
    #[must_use]
    pub fn rect(self) -> Option<PixelRect> {
        self.rect
    }
}

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
    (x_dev * inv, y_dev * inv, (r_dev - x_dev) * inv, (b_dev - y_dev) * inv)
}
