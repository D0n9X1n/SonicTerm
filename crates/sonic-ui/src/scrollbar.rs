//! Pure-function scrollbar geometry model.
//!
//! PR-A of #386: data layer only. No render emit, no input wiring.
//! Consumers (PR-B render, PR-C input) call [`compute`], [`hit_test`],
//! and [`thumb_to_view_top`] without any global state.
//!
//! All coordinates are in physical pixels; the caller is responsible
//! for scaling by the display scale factor.

use sonic_cfg::config::ScrollbarMode;

/// Axis-aligned rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }
}

/// Point in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Resolved geometry for one pane's scrollbar.
///
/// `track_rect` is the full bar; `thumb_rect` is the draggable handle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarGeometry {
    pub track_rect: Rect,
    pub thumb_rect: Rect,
}

/// Result of [`hit_test`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// Outside the track entirely.
    None,
    /// On the track above the thumb (page-up zone).
    TrackAbove,
    /// On the track below the thumb (page-down zone).
    TrackBelow,
    /// On the thumb itself (drag zone).
    Thumb,
}

/// Compute scrollbar geometry for a pane.
///
/// Returns `None` when the scrollbar should not be drawn:
/// - `mode == ScrollbarMode::Never`
/// - `total_rows <= viewport_rows as u64`
/// - `viewport_rows == 0` (degenerate pane)
/// - `width_px <= 0.0` or `pane_rect` has zero area
///
/// `view_top` is the index (0..=total_rows-viewport_rows) of the topmost
/// visible row; 0 = oldest, `total_rows - viewport_rows` = live edge.
pub fn compute(
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    pane_rect: Rect,
    mode: ScrollbarMode,
    width_px: f32,
) -> Option<ScrollbarGeometry> {
    if matches!(mode, ScrollbarMode::Never) {
        return None;
    }
    if viewport_rows == 0 || width_px <= 0.0 || pane_rect.w <= 0.0 || pane_rect.h <= 0.0 {
        return None;
    }
    let vp = viewport_rows as u64;
    if total_rows <= vp {
        return None;
    }
    let total = total_rows;

    let track_w = width_px.min(pane_rect.w);
    let track_rect =
        Rect::new(pane_rect.x + pane_rect.w - track_w, pane_rect.y, track_w, pane_rect.h);

    let ratio = (vp as f32) / (total as f32);
    // Min thumb height keeps the handle grabbable on huge scrollbacks.
    let min_thumb_h = (12.0_f32).min(track_rect.h);
    let thumb_h = (track_rect.h * ratio).max(min_thumb_h).min(track_rect.h);

    let max_view_top = total.saturating_sub(vp);
    let scroll_frac = if max_view_top == 0 {
        0.0
    } else {
        (view_top.min(max_view_top) as f32) / (max_view_top as f32)
    };
    let thumb_y = track_rect.y + scroll_frac * (track_rect.h - thumb_h);

    let thumb_rect = Rect::new(track_rect.x, thumb_y, track_rect.w, thumb_h);
    Some(ScrollbarGeometry { track_rect, thumb_rect })
}

/// Classify a point against a precomputed geometry.
pub fn hit_test(geometry: &ScrollbarGeometry, point: Point) -> HitTarget {
    if !geometry.track_rect.contains(point) {
        return HitTarget::None;
    }
    if geometry.thumb_rect.contains(point) {
        return HitTarget::Thumb;
    }
    if point.y < geometry.thumb_rect.y {
        HitTarget::TrackAbove
    } else {
        HitTarget::TrackBelow
    }
}

/// Translate a thumb-top y coordinate (physical px) back to a `view_top` row.
///
/// Used by PR-C's drag handler: as the cursor moves, the new thumb-y is fed
/// through here to compute the new top-of-view row.
pub fn thumb_to_view_top(
    geometry: &ScrollbarGeometry,
    thumb_y: f32,
    viewport_rows: u16,
    total_rows: u64,
) -> u64 {
    let vp = viewport_rows as u64;
    let max_view_top = total_rows.saturating_sub(vp);
    if max_view_top == 0 {
        return 0;
    }
    let travel = (geometry.track_rect.h - geometry.thumb_rect.h).max(0.0);
    if travel <= 0.0 {
        return 0;
    }
    let dy = (thumb_y - geometry.track_rect.y).clamp(0.0, travel);
    let frac = dy / travel;
    ((frac * max_view_top as f32).round() as i64).clamp(0, max_view_top as i64) as u64
}
