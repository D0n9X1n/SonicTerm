//! Regression test for P0 bug: per-cell ANSI background colors were
//! silently dropped by the renderer. The terminal grid stored `cell.bg`
//! correctly, but the GPU renderer only used a single
//! `LoadOp::Clear(theme.background)` for the whole window and never
//! emitted any per-cell bg quads — so `printf '\033[41mRED\033[0m'`
//! rendered text on the default background, not on red.
//!
//! These tests exercise the `emit_cell_bg_quads` helper directly
//! (it's exposed for testing under `#[doc(hidden)]`) and assert:
//!   1. cells with `bg = Indexed(1)` (ANSI red) produce ≥1 QuadInstance,
//!   2. the quad color is in the right ballpark for red,
//!   3. adjacent same-bg cells are run-length coalesced into ONE quad
//!      (not N quads — the regression that would tank fill-rate on a
//!      simple `\033[41m` 80-col fill),
//!   4. default-bg cells emit nothing (the surface clear handles those).

use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonicterm_gpu::color::srgb_u8_to_linear_lut;
use sonicterm_gpu::core::{emit_cell_bg_quads, emit_cell_bg_quads_clipped};
use sonicterm_gpu::quad::QuadInstance;
use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid};
use sonicterm_ui::pane::Rect as PaneRect;

fn theme_with_red_index1() -> Theme {
    let h = || Hex("#000000".to_string());
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: Hex("#000000".to_string()),
            foreground: Hex("#ffffff".to_string()),
            cursor: h(),
            cursor_text: h(),
            selection_bg: h(),
            selection_fg: h(),
            ansi: AnsiColors {
                black: Hex("#000000".to_string()),
                // ANSI red — what `\033[41m` resolves to.
                red: Hex("#ff0000".to_string()),
                green: Hex("#00ff00".to_string()),
                yellow: Hex("#ffff00".to_string()),
                blue: Hex("#0000ff".to_string()),
                magenta: Hex("#ff00ff".to_string()),
                cyan: Hex("#00ffff".to_string()),
                white: Hex("#ffffff".to_string()),
            },
            bright: AnsiColors {
                black: h(),
                red: h(),
                green: h(),
                yellow: h(),
                blue: h(),
                magenta: h(),
                cyan: h(),
                white: h(),
            },
            tab: TabColors {
                bar_bg: h(),
                active_bg: h(),
                active_fg: h(),
                inactive_bg: h(),
                inactive_fg: h(),
                hover_bg: h(),
                hover_fg: h(),
                close_button_fg: h(),
            },
        },
    }
}

fn write_indexed_run(grid: &mut Grid, row: u16, start_col: u16, len: u16, ch: char, index: u8) {
    let r = grid.row_mut(row);
    for i in 0..len {
        r[(start_col + i) as usize] =
            Cell::plain(ch, Color::Default, Color::Indexed(index), CellFlags::empty());
    }
}

fn write_red_run(grid: &mut Grid, row: u16, start_col: u16, len: u16, ch: char) {
    write_indexed_run(grid, row, start_col, len, ch, 1); // ANSI red — what `\033[41m` sets
}

fn run_emit(grid: &Grid, theme: &Theme) -> Vec<QuadInstance> {
    let mut out = Vec::new();
    // Geometry: 10×20 px cells, 0 pad / 0 top inset, 800×400 screen.
    // The numeric values don't matter for correctness — we only care that
    // SOME quad is emitted at the right run length.
    emit_cell_bg_quads(
        grid,
        grid.scrollback_len() as u64,
        theme,
        0.0,
        0.0,
        10.0,
        20.0,
        800.0,
        400.0,
        &mut out,
    );
    out
}

fn ndc_to_px(rect: [f32; 4], sw: f32, sh: f32) -> (f32, f32, f32, f32) {
    let x = ((rect[0] + 1.0) * 0.5) * sw;
    let w = rect[2] * sw * 0.5;
    let h = rect[3] * sh * 0.5;
    let y = ((1.0 - rect[1] - rect[3]) * 0.5) * sh;
    (x, y, w, h)
}

#[test]
fn red_bg_cells_produce_a_quad_at_all() {
    // The P0 bug: this assertion fails on `main` because the renderer
    // never emitted any bg quads. With the fix it must produce ≥1.
    let mut g = Grid::new(20, 3);
    write_red_run(&mut g, 1, 0, 3, 'A'); // \033[41mAAA\033[0m on row 1
    let quads = run_emit(&g, &theme_with_red_index1());
    assert!(!quads.is_empty(), "expected ≥1 bg quad for red-bg cells, got 0 (P0 regression)");
}

#[test]
fn red_bg_quad_color_is_red_ish() {
    let mut g = Grid::new(20, 3);
    write_red_run(&mut g, 0, 0, 3, 'A');
    let quads = run_emit(&g, &theme_with_red_index1());
    let q = quads.first().expect("at least one quad");
    // Linear-space red: R=1.0, G=0, B=0. Check R dominates and G/B are near zero.
    let [r, g_, b, a] = q.color;
    assert!(r > 0.9, "expected R near 1.0 in linear space, got {r}");
    assert!(g_ < 0.05, "expected G near 0, got {g_}");
    assert!(b < 0.05, "expected B near 0, got {b}");
    assert!((a - 1.0).abs() < 1e-6, "expected alpha 1.0, got {a}");
}

#[test]
fn adjacent_same_bg_cells_coalesce_into_one_quad() {
    // 80 adjacent red-bg cells must produce 1 wide quad, NOT 80 quads.
    // (Otherwise a colorful `htop` would blow the instance buffer.)
    let mut g = Grid::new(80, 1);
    write_red_run(&mut g, 0, 0, 80, 'X');
    let quads = run_emit(&g, &theme_with_red_index1());
    assert_eq!(quads.len(), 1, "expected 1 coalesced quad, got {}", quads.len());
}

#[test]
fn split_runs_separated_by_default_bg_produce_two_quads() {
    // [red red red] [default default] [red red] → 2 quads, not 1 or 5.
    let mut g = Grid::new(20, 1);
    write_red_run(&mut g, 0, 0, 3, 'A');
    write_red_run(&mut g, 0, 5, 2, 'B');
    let quads = run_emit(&g, &theme_with_red_index1());
    assert_eq!(
        quads.len(),
        2,
        "expected 2 quads (two red runs split by default), got {}",
        quads.len()
    );
}

#[test]
fn pure_default_bg_grid_emits_nothing() {
    // Default bg cells must NOT emit quads — surface LoadOp::Clear
    // already covers them. Emitting per-cell would waste fill rate.
    let g = Grid::new(80, 24);
    let quads = run_emit(&g, &theme_with_red_index1());
    assert!(quads.is_empty(), "default-bg cells should emit no quads, got {}", quads.len());
}

#[test]
fn split_pane_background_quads_are_clipped_to_each_pane_rect() {
    let theme = theme_with_red_index1();
    let cell_w = 10.0;
    let cell_h = 20.0;
    let sw = 800.0;
    let sh = 400.0;
    let pane_a = PaneRect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };
    let pane_b = PaneRect { x: 200.0, y: 0.0, w: 200.0, h: 100.0 };

    let mut grid_a = Grid::new(20, 5);
    let mut grid_b = Grid::new(80, 5);
    write_red_run(&mut grid_a, 0, 0, 20, 'A');
    // Deliberately wider than pane_b: regression #263 painted this run
    // into pane A / neighbouring cells instead of clipping to pane_b.
    write_red_run(&mut grid_b, 0, 0, 80, 'B');

    let mut pane_a_quads = Vec::new();
    let mut pane_b_quads = Vec::new();
    emit_cell_bg_quads_clipped(
        &grid_a,
        grid_a.scrollback_len() as u64,
        &theme,
        pane_a,
        cell_w,
        cell_h,
        sw,
        sh,
        &mut pane_a_quads,
    );
    emit_cell_bg_quads_clipped(
        &grid_b,
        grid_b.scrollback_len() as u64,
        &theme,
        pane_b,
        cell_w,
        cell_h,
        sw,
        sh,
        &mut pane_b_quads,
    );

    assert!(!pane_b_quads.is_empty(), "pane B should emit its own quads");
    for q in pane_b_quads {
        let (x, _, w, _) = ndc_to_px(q.rect, sw, sh);
        assert!(x >= pane_b.x - 0.01, "pane B quad started inside pane A: x={x}");
        assert!(
            x + w <= pane_b.x + pane_b.w + 0.01,
            "pane B quad exceeded pane B rect: x={x} w={w} pane_b={pane_b:?}"
        );
    }
}

#[test]
fn rgb_bg_also_emits_quad() {
    // Truecolor `\033[48;2;200;50;50m` → Color::Rgb(200, 50, 50).
    let mut g = Grid::new(10, 1);
    let r = g.row_mut(0);
    for cell in r.iter_mut().take(5) {
        *cell = Cell::plain('x', Color::Default, Color::Rgb(200, 50, 50), CellFlags::empty());
    }
    let quads = run_emit(&g, &theme_with_red_index1());
    assert_eq!(quads.len(), 1, "expected 1 RGB-bg quad, got {}", quads.len());
    let q = &quads[0];
    assert!(q.color[0] > q.color[1] && q.color[0] > q.color[2], "expected red-dominant color");
}

#[test]
fn xterm_256_indexed_backgrounds_emit_expected_colors() {
    // SGR 48;5;n maps to Color::Indexed(n). Indices beyond 15 must resolve
    // through the xterm 256-color palette, not disappear as `None`.
    let cases = [
        (196, [255, 0, 0]),     // bright red from 6×6×6 cube
        (46, [0, 255, 0]),      // bright green from 6×6×6 cube
        (231, [255, 255, 255]), // white-ish cube endpoint
        (240, [88, 88, 88]),    // 24-step grayscale ramp: (240 - 232) * 10 + 8
    ];

    let lut = srgb_u8_to_linear_lut();
    for (col, (idx, expected)) in cases.into_iter().enumerate() {
        let mut g = Grid::new(4, 1);
        write_indexed_run(&mut g, 0, col as u16, 1, 'X', idx);
        let quads = run_emit(&g, &theme_with_red_index1());
        assert_eq!(quads.len(), 1, "expected one bg quad for indexed color {idx}");
        let q = &quads[0];
        let expected =
            [lut[expected[0] as usize], lut[expected[1] as usize], lut[expected[2] as usize], 1.0];
        for (component, expected_component) in expected.iter().enumerate() {
            assert!(
                (q.color[component] - expected_component).abs() < 1e-6,
                "indexed color {idx} component {component}: expected {}, got {}",
                expected_component,
                q.color[component]
            );
        }
    }
}
