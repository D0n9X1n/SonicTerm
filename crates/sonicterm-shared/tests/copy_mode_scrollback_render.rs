use sonicterm_core::grid::{CellFlags, Color, Grid};
use sonicterm_shared::render::GpuRenderer;
use sonicterm_ui::copy_mode::CopyModeState;

#[test]
fn copy_mode_absolute_cursor_auto_scrolls_and_renders_viewport_relative() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..100 {
        grid.scroll_up(1);
    }
    assert_eq!(grid.scrollback_len(), 100);

    let copy_mode = CopyModeState::new_at((7, 5));
    let new_view_top = GpuRenderer::copy_mode_view_top_after_move(
        &copy_mode,
        &grid,
        Some(grid.scrollback_len() as u64),
    )
    .expect("copy-mode cursor in scrollback should force explicit viewport");
    assert!(new_view_top <= 5);

    let visible_row =
        GpuRenderer::viewport_relative_row(copy_mode.cursor.1, new_view_top, grid.rows)
            .expect("cursor row should be visible after auto-scroll");
    assert_eq!(visible_row as u64, 5 - new_view_top);

    let mut quads = Vec::new();
    let cursor_color = [1.0, 0.0, 0.0, 1.0];
    let cursor_px = GpuRenderer::emit_copy_mode_quads(
        &copy_mode,
        &grid,
        new_view_top,
        0.0,
        0.0,
        10.0,
        20.0,
        800.0,
        480.0,
        [0.0, 0.0, 1.0, 0.5],
        cursor_color,
        &mut quads,
    )
    .expect("cursor overlay should render after auto-scroll");
    assert_eq!(cursor_px, (70.0, f32::from(visible_row) * 20.0));
    assert_eq!(quads.last().expect("cursor quad emitted").color, cursor_color);
}

#[test]
fn copy_mode_selection_uses_absolute_rows_and_clips_to_viewport() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..100 {
        grid.scroll_up(1);
    }

    let mut copy_mode = CopyModeState::new_at((3, 4));
    copy_mode.start_select();
    copy_mode.cursor = (8, 6);

    let mut quads = Vec::new();
    let selection_color = [0.0, 0.0, 1.0, 0.5];
    let cursor_color = [1.0, 0.0, 0.0, 1.0];
    let cursor_px = GpuRenderer::emit_copy_mode_quads(
        &copy_mode,
        &grid,
        5,
        0.0,
        0.0,
        10.0,
        20.0,
        800.0,
        480.0,
        selection_color,
        cursor_color,
        &mut quads,
    )
    .expect("cursor row 6 should be visible when view starts at 5");

    assert_eq!(cursor_px, (80.0, 20.0));
    let selection_quads = quads.iter().filter(|quad| quad.color == selection_color).count();
    assert_eq!(selection_quads, 2, "row 4 must be clipped; rows 5 and 6 render");
}

#[test]
fn copy_mode_cursor_overlay_suppressed_when_absolute_row_off_viewport() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..100 {
        grid.scroll_up(1);
    }

    let copy_mode = CopyModeState::new_at((7, 5));
    let mut quads = Vec::new();
    let cursor_px = GpuRenderer::emit_copy_mode_quads(
        &copy_mode,
        &grid,
        grid.scrollback_len() as u64,
        0.0,
        0.0,
        10.0,
        20.0,
        800.0,
        480.0,
        [0.0, 0.0, 1.0, 0.5],
        [1.0, 0.0, 0.0, 1.0],
        &mut quads,
    );

    assert!(cursor_px.is_none());
    assert!(quads.is_empty());
}

#[test]
fn copy_mode_selection_renders_scrollback_content_not_live_viewport() {
    let mut grid = Grid::new(4, 2);
    grid.put_char('S', Color::Default, Color::Default, CellFlags::empty());
    grid.scroll_up(1);
    grid.goto(0, 0);
    grid.put_char('L', Color::Default, Color::Default, CellFlags::empty());

    let mut copy_mode = CopyModeState::new_at((0, 0));
    copy_mode.start_select();
    copy_mode.cursor = (0, 0);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(grid.row_at_abs(0).expect("scrollback row")[0].ch, 'S');
    assert_eq!(grid.row_at_abs(1).expect("live row")[0].ch, 'L');
    assert_eq!(GpuRenderer::viewport_relative_row(0, 0, grid.rows), Some(0));
}
