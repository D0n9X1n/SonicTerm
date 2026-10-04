/// Axis-aligned rectangle in window-pixel space (origin top-left, y grows down)
/// — the common geometry primitive shared between layout code and the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Named by callers outside this crate.
#[allow(clippy::min_ident_chars)]
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
        let left = self.x.max(bounds.x);
        let top = self.y.max(bounds.y);
        let right = self.right().min(bounds.right());
        let bottom = self.bottom().min(bounds.bottom());
        if right <= left || bottom <= top {
            // When: `right <= left` or `bottom <= top`, the clipped rectangles share no positive pixel span.
            return None;
        }
        Some(PixelRect { x: left, y: top, w: (right - left) as u32, h: (bottom - top) as u32 })
    }

    /// Return the smallest rectangle containing both rectangles.
    #[must_use]
    pub fn union(self, other: PixelRect) -> PixelRect {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        // Widen the span to i64 before subtracting. At extreme coordinates
        // (e.g. `left == i32::MIN` while `right` has saturated to `i32::MAX`) the
        // `right - left` span exceeds `i32::MAX` and a narrow `i32` subtraction
        // would overflow-panic in a debug build. Computing in i64 and then
        // clamping back into the `u32` dimension range keeps the result
        // identical for every in-range input while making the extreme case
        // saturate instead of panic.
        let width = (i64::from(right) - i64::from(left)).clamp(0, i64::from(u32::MAX)) as u32;
        let height = (i64::from(bottom) - i64::from(top)).clamp(0, i64::from(u32::MAX)) as u32;
        PixelRect { x: left, y: top, w: width, h: height }
    }
}

/// Padded pane bounds and the complete-row text region in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneContentGeometry {
    /// Unshifted padded region for pane chrome.
    pub content: sonicterm_ui::pane::Rect,
    /// Text region with only the fractional-row remainder moved above it.
    pub grid: sonicterm_ui::pane::Rect,
}

/// Resolve bottom-aligned text without changing pane chrome or terminal row counts.
pub fn pane_content_geometry(
    pane: PixelRect,
    padding: [f32; 4],
    cell_height: f32,
    rows: u16,
) -> PaneContentGeometry {
    let [left, right, top, bottom] = padding;
    let content = sonicterm_ui::pane::Rect::new(
        pane.x as f32 + left,
        pane.y as f32 + top,
        (pane.w as f32 - left - right).max(0.0),
        (pane.h as f32 - top - bottom).max(0.0),
    );
    let fit = (content.h / cell_height).floor();
    let shift = if cell_height.is_finite()
        && cell_height > 0.0
        && fit >= 1.0
        && rows > 0
        && f32::from(rows) <= fit
    {
        // Resource-limited grids must not consume whole empty rows as alignment slack.
        (content.h - fit * cell_height).max(0.0).floor()
    } else {
        // When: rows exceed the truncated pane or no complete row fits, retain its existing origin.
        0.0
    };
    let grid =
        sonicterm_ui::pane::Rect::new(content.x, content.y + shift, content.w, content.h - shift);
    PaneContentGeometry { content, grid }
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
        let Some(clipped) = rect.intersect(bounds) else {
            // When: `rect` has no overlap with `bounds`, it contributes no frame damage.
            return;
        };
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

/// Exact pixel area covered by the union of `parts`, counting every pixel once.
///
/// Coordinate compression over the parts' x edges: within each x slab, the parts spanning it
/// contribute y intervals that are merged and summed. The cost is quadratic in the part count,
/// which stays small (a frame's damage contributions). Empty parts cover nothing.
#[must_use]
pub fn covered_area(parts: &[PixelRect]) -> u64 {
    // Edges are widened to i64 so a saturated right or bottom edge cannot overflow the span.
    let spans: Vec<[i64; 4]> = parts
        .iter()
        .filter(|part| !part.is_empty())
        .map(|part| {
            [i64::from(part.x), i64::from(part.y), i64::from(part.right()), i64::from(part.bottom())]
        })
        .collect();
    let mut x_edges: Vec<i64> = spans.iter().flat_map(|span| [span[0], span[2]]).collect();
    x_edges.sort_unstable();
    x_edges.dedup();
    let mut area: u64 = 0;
    let mut y_intervals: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for slab in x_edges.windows(2) {
        let (slab_left, slab_right) = (slab[0], slab[1]);
        y_intervals.clear();
        y_intervals.extend(
            spans
                .iter()
                .filter(|span| span[0] <= slab_left && span[2] >= slab_right)
                .map(|span| (span[1], span[3])),
        );
        y_intervals.sort_unstable();
        let mut covered_height: i64 = 0;
        let mut open: Option<(i64, i64)> = None;
        for &(top, bottom) in &y_intervals {
            open = match open {
                Some((open_top, open_bottom)) if top <= open_bottom => {
                    Some((open_top, open_bottom.max(bottom)))
                }
                Some((open_top, open_bottom)) => {
                    // When: this interval starts past the open one, the open run is complete.
                    covered_height += open_bottom - open_top;
                    Some((top, bottom))
                }
                None => Some((top, bottom)),
            };
        }
        if let Some((open_top, open_bottom)) = open {
            covered_height += open_bottom - open_top;
        }
        let slab_width = (slab_right - slab_left).unsigned_abs();
        area = area.saturating_add(slab_width.saturating_mul(covered_height.unsigned_abs()));
    }
    area
}

/// Snap a logical-space `(left, top, width, height)` rect so that its four edges
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
/// derive width and height as their differences. Snapping `width` and `height`
/// independently of `left`/`top` would accumulate up-to-±0.5-device-pixel
/// drift across a row, leaving visible gaps or overlaps between
/// adjacent glyph quads.
pub fn snap_to_device_pixels(rect: (f32, f32, f32, f32), scale: f32) -> (f32, f32, f32, f32) {
    let (left, top, width, height) = rect;
    // Integer-scale fast path — see module doc.
    if scale.fract() == 0.0 {
        // When: integer `scale` already aligns the established layout and rounding would shift font-derived edges.
        return rect;
    }
    let x_dev = (left * scale).round();
    let y_dev = (top * scale).round();
    let r_dev = ((left + width) * scale).round();
    let b_dev = ((top + height) * scale).round();
    let inv = 1.0 / scale;
    (x_dev * inv, y_dev * inv, (r_dev - x_dev) * inv, (b_dev - y_dev) * inv)
}

#[cfg(test)]
#[path = "geometry_tests.rs"]
mod geometry_tests;
