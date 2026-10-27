//! Backgrounds responsibilities of the renderer, preserving its single owner.

use super::*;

/// Resolve a cell's background to a linear-space `[r,g,b,a]` suitable for the
/// quad pipeline, OR `None` if the cell should fall through to the surface
/// clear color (`theme.colors.background`).
///
/// Returning `None` for the default-bg case lets the per-row emit loop skip
/// pushing a quad over every blank cell — the attachment clear or partial
/// replacement reset already supplies the configured background.
///
/// Note on color space: the wgpu surface is `Bgra8UnormSrgb`, so the quad
/// fragment shader's output is sRGB-encoded on write. Inputs MUST therefore
/// be in linear-light space, otherwise gamma is applied twice and the result
/// looks washed out (same trap documented in `color.rs::hex_to_premultiplied_rgba`). The
/// sRGB→linear LUT here is bit-exact with the one feeding `hex_to_premultiplied_rgba`, so
/// `Color::Indexed(1)` (ANSI red) ends up identical to the theme's `ansi.red`
/// rendered through the LoadOp clear path.
#[doc(hidden)]
pub fn cell_bg_rgba(cell: &Cell, theme: &Theme) -> Option<[f32; 4]> {
    let color = if cell.flags.contains(CellFlags::INVERSE) {
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        color_to_chrome(cell.fg, theme, default_fg)
    } else {
        // When: INVERSE is clear, so the cell keeps its own bg and the glyph keeps fg; swapping them here too would cancel out reverse-video runs.
        match cell.bg {
            Color::Default => {
                // When: `Color::Default` is used, the attachment clear or replacement reset supplies this cell's background.
                return None;
            }
            bg => color_to_chrome(bg, theme, ChromeColor::rgb(0, 0, 0)),
        }
    };
    let lut = super::super::color::srgb_u8_to_linear_lut();
    Some([lut[color.r() as usize], lut[color.g() as usize], lut[color.b() as usize], 1.0])
}

/// Walk the visible rows of `grid`, emit one `QuadInstance` per maximal run
/// of horizontally-adjacent cells that share the same non-default background
/// color. Cells whose `bg` resolves to the theme default are skipped — the
/// attachment clear or partial replacement reset already covers them.
///
/// Run-length coalescing is essential: a single `\033[41m` color-fill of an
/// 80-column row would otherwise produce 80 quads where 1 suffices. The
/// renderer can hit tens of thousands of background cells per frame during
/// e.g. `htop` or `vim` syntax highlighting; per-cell quads would blow the
/// instance buffer and tank fill-rate.
///
/// `WIDE_CONT` cells (the right half of a wide CJK cell) inherit the lead
/// cell's bg via the parser, so they participate in the same run naturally.
///
/// The emitted quads are sharp-edged (no SDF) and pushed onto `out` in row-
/// major order. Caller is responsible for placing this BEFORE selection /
/// cursor / overlay quads in the draw vector so those still paint on top.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)] // mirrors flush_shape_run / collect_hyperlink_runs siblings — all geometry must be threaded in explicitly to keep this a free function (testable without a full GpuRenderer)
pub fn emit_cell_bg_quads(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    emit_cell_bg_quads_clipped(
        grid,
        view_top_abs,
        theme,
        PaneRect {
            x: pad,
            y: top_inset,
            w: f32::from(grid.cols) * cell_w,
            h: f32::from(grid.rows) * cell_h,
        },
        cell_w,
        cell_h,
        sw,
        sh,
        out,
    );
}

/// Like [`emit_cell_bg_quads`] but clips runs to a pane sub-rect. This is
/// the production split-pane path: a pane whose grid is wider than its
/// current tile must never emit quads into its neighbour's rectangle.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_clipped(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pane_rect: PaneRect,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    let pad = pane_rect.x;
    let top_inset = pane_rect.y;
    let max_cols = ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(grid.cols)) as u16;
    let max_rows = ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(grid.rows)) as u16;
    if max_cols == 0 || max_rows == 0 {
        // When: max_cols or max_rows floors to zero, the pane tile cannot hold one whole cell; emitting anyway would paint bg into the neighbour pane.
        return;
    }
    // Snap this pane's raster-pixel edges so its background aligns with its glyphs, not the active pane's grid.
    let snapped_cell_x = build_snapped_cell_x(pad, cell_w, grid.cols);
    for r in 0..max_rows {
        emit_cell_bg_quads_for_row(
            grid,
            view_top_abs,
            theme,
            pad,
            top_inset,
            cell_w,
            cell_h,
            sw,
            sh,
            max_cols,
            r,
            out,
            &snapped_cell_x,
        );
    }
}

/// The pane-focus flash: the pane's chrome rectangle, lifted 0.07 above the background, at `alpha`.
pub(crate) fn focus_flash_quad(
    bg_rgba: [f32; 4],
    chrome: (f32, f32, f32, f32),
    alpha: f32,
    (sw, sh): (f32, f32),
) -> QuadInstance {
    let flash_rgb =
        [(bg_rgba[0] + 0.07).min(1.0), (bg_rgba[1] + 0.07).min(1.0), (bg_rgba[2] + 0.07).min(1.0)];
    QuadInstance {
        rect: px_to_ndc(chrome.0, chrome.1, chrome.2, chrome.3, sw, sh),
        color: premultiply([flash_rgb[0], flash_rgb[1], flash_rgb[2], alpha]),
        ..Default::default()
    }
}

/// Where a selection is drawn: its pane's viewport, grid size, origin, cells, clip and surface.
pub(crate) struct SelectionGeometry {
    pub(crate) view_top_abs: u64,
    pub(crate) grid_size: (u16, u16),
    pub(crate) origin: (f32, f32),
    pub(crate) cell_size: (f32, f32),
    pub(crate) clip: (f32, f32, f32, f32),
    pub(crate) surface: (f32, f32),
}

/// Push the selection highlight quads, clipped to the pane so a drag across a split cannot bleed.
pub(crate) fn push_selection_quads(
    out: &mut Vec<QuadInstance>,
    sel: &sonicterm_render_model::boundary::ui::selection::Selection,
    geometry: &SelectionGeometry,
    snapped_cell_x: &[f32],
    color: [f32; 4],
) {
    let (clip_x, clip_y, clip_w, clip_h) = geometry.clip;
    for rect in selection_quad_rects(
        sel,
        geometry.view_top_abs,
        geometry.grid_size.0,
        geometry.grid_size.1,
        geometry.origin.0,
        geometry.origin.1,
        geometry.cell_size.0,
        geometry.cell_size.1,
        snapped_cell_x,
    )
    .into_iter()
    .filter_map(|rect| clip_rect_to_pane(rect, clip_x, clip_y, clip_w, clip_h))
    {
        out.push(QuadInstance {
            rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, geometry.surface.0, geometry.surface.1),
            color,
            ..Default::default()
        });
    }
}

/// Pane geometry one row's background quads depend on, in raster pixels.
pub(crate) struct RowBackgroundGeometry {
    pub(crate) origin: (f32, f32),
    pub(crate) pane_size: (f32, f32),
    pub(crate) cell_size: (f32, f32),
    pub(crate) surface: (f32, f32),
    pub(crate) max_cols: u16,
}

/// Which row of which pane is being emitted.
pub(crate) struct RowBackgroundRow<'grid> {
    pub(crate) pane_id: crate::row_quad_cache::PaneId,
    pub(crate) grid: &'grid Grid,
    pub(crate) view_top_abs: u64,
    pub(crate) slot: u16,
}

/// Append one row's background quads to `out`, replaying them from `cache` when the row's key
/// (contents, slot, style, geometry and selection overlap) matches, else emitting and caching
/// them. Returns whether the row was replayed. Grid dirt is not consulted: a key that covers
/// every input is what makes a replay equal a fresh emission.
pub(crate) fn emit_row_background<'cell, Cells>(
    cache: &mut crate::row_quad_cache::LineQuadCache,
    row: RowBackgroundRow<'_>,
    cells: Cells,
    (style_rev, theme, selection): (u64, &Theme, Option<(u64, u16, u64, u16)>),
    geometry: &RowBackgroundGeometry,
    snapped_cell_x: &[f32],
    out: &mut Vec<QuadInstance>,
) -> bool
where
    Cells: IntoIterator<Item = &'cell Cell>,
{
    let row_abs = row.view_top_abs + u64::from(row.slot);
    // Cell dimensions already encode DPI in raster pixels; the separate scale key stays 1.0.
    let key = crate::row_quad_cache::row_quad_hash_cells(
        row.view_top_abs,
        row.slot as usize,
        cells,
        style_rev,
        geometry.cell_size.0,
        geometry.cell_size.1,
        geometry.origin.0,
        geometry.origin.1,
        geometry.pane_size.0,
        geometry.pane_size.1,
        selection,
    );
    if let Some(cached) = cache.get(row.pane_id, row_abs, key) {
        // When: `cache.get` is Some — the row's contents, slot, style, geometry and selection overlap are unchanged.
        out.extend_from_slice(&cached.quads);
        return true;
    }
    let base = out.len();
    emit_cell_bg_quads_for_row(
        row.grid,
        row.view_top_abs,
        theme,
        geometry.origin.0,
        geometry.origin.1,
        geometry.cell_size.0,
        geometry.cell_size.1,
        geometry.surface.0,
        geometry.surface.1,
        geometry.max_cols,
        row.slot,
        out,
        snapped_cell_x,
    );
    let quads = out[base..].to_vec();
    cache.insert(row.pane_id, row_abs, key, crate::row_quad_cache::CachedRowQuads { quads });
    false
}

/// Emit background quads for a single visible row. Extracted so the
/// `LineQuadCache` miss path (P2) can call it for one row
/// at a time and capture the resulting quads into the cache.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_for_row(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    max_cols: u16,
    r: u16,
    out: &mut Vec<QuadInstance>,
    snapped_cell_x: &[f32],
) {
    {
        let row_abs = view_top_abs + r as u64;
        let Some(row) = grid.row_at_abs(row_abs) else {
            // When: row_at_abs finds no row for row_abs, that absolute line has aged out of scrollback, so this viewport row has no cells to shade.
            return;
        };
        // Run-length encode adjacent same-bg cells into one quad.
        let mut run_start: Option<u16> = None;
        let mut run_color: Option<[f32; 4]> = None;
        let mut col: u16 = 0;
        // derive x/w from the shared snapped-edge cache so bg
        // runs share device-pixel edges with adjacent glyph cells at
        // fractional DPI. Falls back to raw arithmetic if the cache is
        // empty (defensive — production always passes the full cache).
        let raw_fallback = snapped_cell_x.is_empty();
        let flush =
            |start: u16, end_exclusive: u16, color: [f32; 4], out: &mut Vec<QuadInstance>| {
                let clipped_end = end_exclusive.min(max_cols);
                if clipped_end <= start {
                    // When: max_cols pulls clipped_end back to or before start, the run lies outside the pane tile and would push a zero-width quad.
                    return;
                }
                let (x, w) = if raw_fallback {
                    (pad + f32::from(start) * cell_w, f32::from(clipped_end - start) * cell_w)
                } else {
                    // When: raw_fallback is off, the run takes its edges from the shared snapped cache so bg meets the glyph cells exactly at fractional DPI.
                    let lo = snapped_cell_x[start as usize];
                    let hi = snapped_cell_x[clipped_end as usize];
                    (lo, hi - lo)
                };
                let y = top_inset + f32::from(r) * cell_h;
                out.push(QuadInstance::sharp(px_to_ndc(x, y, w, cell_h, sw, sh), color));
            };
        for cell in row.iter().take(max_cols as usize) {
            let bg = cell_bg_rgba(cell, theme);
            match (run_color, bg) {
                (Some(prev), Some(cur)) if prev == cur => {
                    // When: cur repeats prev, so the cell joins the open run and an 80-column fill stays one quad instead of eighty.
                    // extend run
                }
                (Some(prev), _) => {
                    // PANIC: safe — `run_color` and `run_start` are written
                    // together (search this fn for `run_start = ` to see they
                    // are always assigned in the same statement-pair). Matching
                    // `run_color == Some(_)` therefore proves `run_start ==
                    // Some(_)`. Hot per-frame path: no Result conversion.
                    let start = run_start.expect("run_start set when run_color is");
                    flush(start, col, prev, out);
                    run_start = bg.map(|_| col);
                    run_color = bg;
                }
                (None, Some(_)) => {
                    run_start = Some(col);
                    run_color = bg;
                }
                (None, None) => {
                    // When: `run_color` and `bg` are absent, the attachment clear or replacement reset already covers this cell.
                }
            }
            col = col.saturating_add(1);
        }
        if let (Some(start), Some(color)) = (run_start, run_color) {
            flush(start, col, color, out);
        }
    }
}

pub(super) fn indexed(i: u8, theme: &Theme) -> Option<ChromeColor> {
    let p = &theme.colors;
    let pick = |h: &str| hex_to_chrome_color(h);
    match i {
        0 => Some(pick(p.ansi.black.0.as_str())),
        1 => Some(pick(p.ansi.red.0.as_str())),
        2 => Some(pick(p.ansi.green.0.as_str())),
        3 => Some(pick(p.ansi.yellow.0.as_str())),
        4 => Some(pick(p.ansi.blue.0.as_str())),
        5 => Some(pick(p.ansi.magenta.0.as_str())),
        6 => Some(pick(p.ansi.cyan.0.as_str())),
        7 => Some(pick(p.ansi.white.0.as_str())),
        8 => Some(pick(p.bright.black.0.as_str())),
        9 => Some(pick(p.bright.red.0.as_str())),
        10 => Some(pick(p.bright.green.0.as_str())),
        11 => Some(pick(p.bright.yellow.0.as_str())),
        12 => Some(pick(p.bright.blue.0.as_str())),
        13 => Some(pick(p.bright.magenta.0.as_str())),
        14 => Some(pick(p.bright.cyan.0.as_str())),
        15 => Some(pick(p.bright.white.0.as_str())),
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v / 6) % 6;
            let b = v % 6;
            // When: c is nonzero, the xterm 6x6x6 cube spaces levels at 55 + 40c rather than evenly, so 256-color ramps match other terminals.
            let to8bit = |c: u8| if c == 0 { 0 } else { c * 40 + 55 };
            Some(ChromeColor::rgb(to8bit(r), to8bit(g), to8bit(b)))
        }
        232..=255 => {
            let g = (i - 232) * 10 + 8;
            Some(ChromeColor::rgb(g, g, g))
        }
    }
}
