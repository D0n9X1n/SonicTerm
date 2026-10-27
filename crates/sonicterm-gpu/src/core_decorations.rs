//! Decorations responsibilities of the renderer, preserving its single owner.

use super::*;

/// Emit a pane's scrollbar (track + thumb) into `quads_overlay` using the
/// shared geometry model. No-op when the pane has nothing to scroll, the
/// mode is `Never`, or `alpha` is at or below the emit floor.
/// Returns the number of quads emitted (for tests).
///
/// `alpha` in `[0.0, 1.0]` scales both track + thumb tint alphas; the
/// caller (app loop) feeds the lerped per-pane fade value from
/// `sonicterm_app::app::scrollbar_visibility::tick`.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_pane_scrollbar(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rect: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    theme: &Theme,
    sw: f32,
    sh: f32,
    alpha: f32,
    scale: f32,
) -> usize {
    if alpha <= sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR {
        // When: `alpha` is at the fade floor where the bar is invisible —
        // emitting costs two quads per pane per frame for unseeable pixels.
        return 0;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    // The planner damages the same track, so drawing and damage cannot diverge.
    let Some(geom) = crate::frame_plan::pane_scrollbar_geometry(
        pane_rect,
        viewport_rows,
        total_rows,
        view_top,
        mode,
        scale,
    ) else {
        // When: `scrollbar::compute` yields None — nothing beyond the viewport
        // to scroll, or mode is `Never`. No track or thumb to place.
        return 0;
    };
    let fg_hex = theme.colors.foreground.0.as_str();
    let track_color = scrollbar_tint(fg_hex, 0.10 * alpha);
    let thumb_color = scrollbar_tint(fg_hex, 0.30 * alpha);
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.track_rect.x,
            geom.track_rect.y,
            geom.track_rect.w,
            geom.track_rect.h,
            sw,
            sh,
        ),
        track_color,
    ));
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.thumb_rect.x,
            geom.thumb_rect.y,
            geom.thumb_rect.w,
            geom.thumb_rect.h,
            sw,
            sh,
        ),
        thumb_color,
    ));
    2
}

pub(super) fn splitter_rects_from_panes(
    pane_rects: &[(u64, PaneRect)],
    thickness: f32,
) -> Vec<SplitterRect> {
    let mut out = Vec::new();
    let thickness = thickness.max(0.0);
    let eps = 0.5_f32;

    for (i, (_, a)) in pane_rects.iter().enumerate() {
        for (_, b) in pane_rects.iter().skip(i + 1) {
            let vertical_overlap = a.y.max(b.y) < (a.y + a.h).min(b.y + b.h) - eps;
            if vertical_overlap && ((a.x + a.w) - b.x).abs() <= eps {
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(b.x - thickness * 0.5, y, thickness, h),
                });
            } else if vertical_overlap && ((b.x + b.w) - a.x).abs() <= eps {
                // When: `(b.x + b.w) - a.x` is the contact instead — the pair
                // walk is unordered, so the splitter straddles a's left edge.
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(a.x - thickness * 0.5, y, thickness, h),
                });
            }

            let horizontal_overlap = a.x.max(b.x) < (a.x + a.w).min(b.x + b.w) - eps;
            if horizontal_overlap && ((a.y + a.h) - b.y).abs() <= eps {
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, b.y - thickness * 0.5, w, thickness),
                });
            } else if horizontal_overlap && ((b.y + b.h) - a.y).abs() <= eps {
                // When: `(b.y + b.h) - a.y` is the contact — mirrored pair
                // order, so the splitter straddles a's top edge.
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, a.y - thickness * 0.5, w, thickness),
                });
            }
        }
    }

    out
}

pub(super) fn emit_broadcast_borders(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rects: &[(u64, PaneRect)],
    participants: &[u64],
    warning: [f32; 4],
    sw: f32,
    sh: f32,
) {
    for (id, r) in pane_rects {
        if !participants.contains(id) || r.w <= 0.0 || r.h <= 0.0 {
            // When: id is not a participant or r has no area, no safety edge belongs in this pane.
            continue;
        }
        // Physical-pixel edges stay thin regardless of font size and cannot overlap on tiny panes.
        let t = 2.0_f32.min(r.w / 2.0).min(r.h / 2.0);
        for rect in [
            PaneRect::new(r.x, r.y, r.w, t),
            PaneRect::new(r.x, r.y + r.h - t, r.w, t),
            PaneRect::new(r.x, r.y + t, t, r.h - 2.0 * t),
            PaneRect::new(r.x + r.w - t, r.y + t, t, r.h - 2.0 * t),
        ] {
            quads_overlay.push(QuadInstance::sharp(
                px_to_ndc(rect.x, rect.y, rect.w, rect.h, sw, sh),
                warning,
            ));
        }
    }
}

pub(super) fn fold_u64_to_u32(value: u64) -> u32 {
    ((value >> 32) as u32) ^ (value as u32)
}

pub(super) fn underline_key(cell: &Cell) -> Option<(UnderlineStyle, Color)> {
    cell.flags
        .contains(CellFlags::UNDERLINE)
        .then(|| (cell.underline_style(), cell.underline_color().unwrap_or(cell.fg)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_underline_quads(
    out: &mut Vec<QuadInstance>,
    style: UnderlineStyle,
    x: f32,
    y: f32,
    w: f32,
    cell_h: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    if w <= 0.0 {
        // When: `w <= 0.0` — an empty or inverted span, which would emit a
        // quad with no area.
        return;
    }
    let bottom_y = y + cell_h - thickness;
    match style {
        UnderlineStyle::Single => {
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Double => {
            let gap = thickness.max(1.0);
            let y1 = (bottom_y - gap - thickness).max(y);
            out.push(QuadInstance::sharp(px_to_ndc(x, y1, w, thickness, sw, sh), color));
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Dotted => {
            let dot = (thickness * 1.6).max(1.0);
            let step = dot * 2.0;
            let mut dx = 0.0;
            while dx < w {
                let size = dot.min(w - dx);
                out.push(QuadInstance::rounded(
                    px_to_ndc(x + dx, bottom_y, size, dot, sw, sh),
                    color,
                    [size, dot],
                    dot * 0.5,
                ));
                dx += step;
            }
        }
        UnderlineStyle::Dashed => {
            let dash = (thickness * 4.0).max(4.0);
            let gap = (thickness * 2.0).max(2.0);
            let mut dx = 0.0;
            while dx < w {
                let len = dash.min(w - dx);
                out.push(QuadInstance::sharp(
                    px_to_ndc(x + dx, bottom_y, len, thickness, sw, sh),
                    color,
                ));
                dx += dash + gap;
            }
        }
        UnderlineStyle::Curly => {
            let amp = (thickness * 1.4).max(1.0);
            let step = (thickness * 4.0).max(4.0);
            let mid_y = y + cell_h - thickness - amp;
            let mut sx = x;
            let mut up = true;
            while sx < x + w {
                let ex = (sx + step).min(x + w);
                // When: up flips once per segment, so each curl stroke starts where the previous ended and the row reads as one wave, not dashes.
                let sy = if up { mid_y + amp } else { mid_y - amp };
                // When: up drives the end point to the opposite side of mid_y, giving this segment the inverse slope of its neighbour.
                let ey = if up { mid_y - amp } else { mid_y + amp };
                push_line_segment_px(out, sx, sy, ex, ey, thickness, sw, sh, color);
                sx = ex;
                up = !up;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_line_segment_px(
    out: &mut Vec<QuadInstance>,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    let pad = thickness * 0.5 + 1.0;
    let x0 = ax.min(bx) - pad;
    let y0 = ay.min(by) - pad;
    let x1 = ax.max(bx) + pad;
    let y1 = ay.max(by) + pad;
    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);
    let cx = x0 + w * 0.5;
    let cy = y0 + h * 0.5;
    out.push(QuadInstance::line(
        px_to_ndc(x0, y0, w, h, sw, sh),
        color,
        [w, h],
        [ax - cx, ay - cy],
        [bx - cx, by - cy],
        thickness,
    ));
}

// `terminal_font_attrs` re-export removed. It returned
// `legacy chrome attrs` which carried per-span family/weight; the
// chrome-text path replaces it with `ChromeAttrs { bold, italic }`
// constructed per-span at the call site. Downstream callers
// (`sonicterm-ui::tab_spans`) build `(text, ChromeColor, ChromeAttrs)`
// span tuples directly. The grid/chrome shape calls reach the loaded
// wezterm font via `FontStack::default_font()` — there is no per-span
// font attribute layer in this path.

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
pub fn collect_hyperlink_runs(grid: &Grid) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut start: Option<u16> = None;
        let mut current: Option<sonicterm_render_model::boundary::grid::hyperlink::HyperlinkId> =
            None;
        let mut last_col: u16 = 0;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: WIDE_CONT marks the trailing half of a wide cell, which inherits the lead cell's hyperlink, so it extends the run instead of breaking it.
                if start.is_some() {
                    last_col = col as u16;
                }
                continue;
            }
            match (cell.hyperlink(), current) {
                (Some(hid), Some(cur)) if hid == cur => {
                    last_col = col as u16;
                }
                (Some(hid), _) => {
                    if let (Some(s), Some(_)) = (start, current) {
                        runs.push((r, s, last_col));
                    }
                    start = Some(col as u16);
                    current = Some(hid);
                    last_col = col as u16;
                }
                (None, Some(_)) => {
                    if let Some(s) = start.take() {
                        runs.push((r, s, last_col));
                    }
                    current = None;
                }
                (None, None) => {
                    // When: the cell carries no hyperlink and current is unset, there is no run to open or close, so the walk just advances.
                }
            }
        }
        if let (Some(s), Some(_)) = (start, current) {
            runs.push((r, s, last_col));
        }
    }
    runs
}

/// Stable fingerprint of a tab's command chrome: the status kind and the badge drawn for it at
/// `now` on a tab whose activity is `is_active`. It changes only when the drawn badge appears,
/// changes or disappears, or when the status kind changes, never on an undrawn elapsed second.
#[doc(hidden)]
pub fn command_status_hash(
    status: &sonicterm_render_model::boundary::ui::tabs::CommandStatus,
    now: Instant,
    is_active: bool,
) -> u64 {
    use sonicterm_render_model::boundary::ui::tabs::CommandStatus;
    use std::hash::{Hash, Hasher};
    // The kind keeps Idle and an unbadged running tab apart; `exit` picks the drawn mark.
    let kind: u8 = match status {
        CommandStatus::Idle => 0,
        CommandStatus::Running(_) => 1,
        CommandStatus::Done { .. } => 2,
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut hash);
    status.clone().badge(now, is_active).hash(&mut hash);
    hash.finish()
}

/// Compute the per-row selection quad rects (in physical pixels) that the
/// renderer would emit for `sel` against a grid of `rows` × `cols`, anchored
/// at `(origin_x, origin_y)` with `cell_w × cell_h` cells.
///
/// Pure helper, no clipping applied — pair with [`clip_rect_to_pane`] before
/// pushing to the GPU. Exposed so integration tests can verify the
/// pre-clip / post-clip relationship without standing up a real surface.
///
/// Each returned tuple is `(x, y, w, h)` in physical pixels.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn selection_quad_rects(
    sel: &sonicterm_render_model::boundary::ui::selection::Selection,
    view_top_abs: u64,
    rows: u16,
    cols: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Vec<(f32, f32, f32, f32)> {
    if sel.is_empty() {
        // When: sel covers no cells, returning an empty vec drops the previous frame's highlight rather than leaving a stale rect on screen.
        return Vec::new();
    }
    let (a, b) = sel.normalized();
    let mut out = Vec::with_capacity(usize::from(rows));
    // derive each row's x/w from the shared snapped-edge cache so
    // selection rects share device-pixel edges with adjacent glyph
    // cells at fractional DPI. Empty-cache fallback preserves the old
    // raw-arithmetic behavior for callers (debug/test helpers) that
    // don't carry a real cache; integer scales make the two identical.
    let raw_fallback = snapped_cell_x.is_empty();
    // Selection rows are scrollback-ABSOLUTE. Only the absolute rows that
    // intersect the viewport produce quads, so bound the walk to
    // `[max(a.0, view_top_abs) ..= min(b.0, view_top_abs + rows - 1)]`.
    // This keeps per-frame cost O(viewport rows) even when the selection
    // spans a huge multi-screen region of scrollback. The first/last-row
    // column tests still compare against the true `a.0`/`b.0` (which may
    // sit off-screen), so partial first/last rows render correctly.
    if rows == 0 {
        // When: rows is zero the viewport has no line to highlight, and `rows as u64 - 1` below would underflow into a bogus bottom bound.
        return out;
    }
    let view_bottom_abs = view_top_abs + (rows as u64 - 1);
    let first_abs = a.0.max(view_top_abs);
    let last_abs = b.0.min(view_bottom_abs);
    if first_abs > last_abs {
        // When: first_abs passes last_abs, no absolute selection row intersects the viewport, so a selection deep in scrollback costs no per-row walk.
        return out; // selection entirely above or below the viewport
    }
    for abs_r in first_abs..=last_abs {
        let vr = (abs_r - view_top_abs) as u16;
        // When: abs_r is past a.0 the row starts mid-selection, so col_a falls back to 0 and the highlight spans from the left edge.
        let col_a = if abs_r == a.0 { a.1 } else { 0 };
        // Note: do NOT clamp `col_b` to `cols - 1` here. The selection may
        // legitimately reach the grid's last column, and the per-pane clip
        // below trims any pixel overhang. Clamping pre-clip would silently
        // shrink the selection on the last row when the user dragged past
        // the rightmost cell — which is precisely the path that hides
        // bugs like the split-pane bleed-through.

        // When: abs_r sits before b.0 the row ends mid-selection, so col_b runs to the last column and the highlight reads as continuous.
        let col_b = if abs_r == b.0 { b.1 } else { cols.saturating_sub(1) };
        if col_b < col_a {
            // When: col_b lands left of col_a the row holds no selected span, and `end_exclusive - col_a` would wrap on u16.
            continue;
        }
        let end_exclusive = col_b.saturating_add(1);
        let (x, w) = if raw_fallback {
            (origin_x + f32::from(col_a) * cell_w, f32::from(end_exclusive - col_a) * cell_w)
        } else {
            // When: raw_fallback is off, the rect takes its edges from the shared snapped cache so selection meets the glyph cells exactly at fractional DPI.

            // Clamp the right edge to the cache bounds (`cols + 1`); a
            // selection that touches col `cols - 1` reads `snapped[cols]`.
            let cache_end = end_exclusive.min((snapped_cell_x.len() - 1) as u16);
            if cache_end <= col_a {
                // When: the cache clamp pulls cache_end back to or before col_a, the row's span falls outside the cached edges and would be zero-width.
                continue;
            }
            let lo = snapped_cell_x[col_a as usize];
            let hi = snapped_cell_x[cache_end as usize];
            (lo, hi - lo)
        };
        let y = origin_y + f32::from(vr) * cell_h;
        out.push((x, y, w, cell_h));
    }
    out
}

/// Clip a quad rect (in physical pixels) to the active pane's bounding box.
/// Returns `None` if the rect is entirely outside the pane.
///
/// Selection / cursor / overlay quads are anchored to the active pane's
/// origin and can extend past its right or bottom edge when the user drags
/// beyond the pane (or the cursor temporarily sits outside the grid bounds
/// due to a resize race). Pushing the unclipped quad would paint into the
/// neighbouring pane in a split layout — see the regression test for
/// the split-right drag-select bug.
#[doc(hidden)]
pub fn clip_rect_to_pane(
    rect: (f32, f32, f32, f32),
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = rect;
    let clipped_x = x.max(pane_x);
    let clipped_right = (x + w).min(pane_x + pane_w);
    let clipped_y = y.max(pane_y);
    let clipped_bottom = (y + h).min(pane_y + pane_h);
    let clipped_w = clipped_right - clipped_x;
    let clipped_h = clipped_bottom - clipped_y;
    if clipped_w > 0.0 && clipped_h > 0.0 {
        Some((clipped_x, clipped_y, clipped_w, clipped_h))
    } else {
        // When: clipped_w or clipped_h collapses to zero, the rect lies wholly outside the pane; returning nothing keeps it out of the neighbour's tile.
        None
    }
}

#[cfg(test)]
#[path = "core_decorations_tests.rs"]
mod core_decorations_tests;
