use super::vt_test_support::*;
use super::*;

// Media-capture tests stage in a private `CaptureStagingPool`, never the
// process-default pool: `StagingReservation::admit` panics under this crate's
// unit tests if one tries, so no test can perturb a sibling's measurement.

#[test]
fn parser_resize_reconciles_fullscreen_margins_in_both_directions() {
    // Absolute margins from even a bare reset cannot survive a changed terminal size.
    for (initial_rows, rows) in [(6, 4), (4, 6)] {
        for setup in
            ["\x1b[r".to_owned(), "\x1b[0;0r".to_owned(), format!("\x1b[1;{initial_rows}r")]
        {
            for control in ["\n", "\x0b", "\x0c", "\x1bD", "\x1bE"] {
                let mut parser = Parser::new(Grid::new(12, initial_rows));
                parser.advance(setup.as_bytes());
                parser.advance(b"\x1b[2;3H");
                parser.resize(12, rows);
                assert_eq!(parser.grid().cursor, sonicterm_grid::grid::Pos { row: 1, col: 2 });
                assert_eq!(parser.performer.effective_scroll_region(), (0, rows - 1));
                parser.advance(format!("\x1b[{rows};1H").as_bytes());
                for line_number in 0..3 {
                    parser.advance(format!("line{line_number:02}\r{control}").as_bytes());
                }
                assert_eq!(
                    parser.grid().scrollback_len(),
                    3,
                    "setup={setup:?}, control={control:?}, rows={rows}"
                );
                assert_eq!(row_text(&parser, rows - 2).trim_end(), "line02");
            }
        }
    }
}

#[test]
fn parser_resize_preserves_partial_margins_on_bounded_noop() {
    // A repeated layout, including a clamped request, must not erase an application's valid region.
    for (cols, requested_cols) in [(12, 12), (sonicterm_grid::grid::MAX_GRID_AXIS, u16::MAX)] {
        let mut parser = Parser::new(Grid::new(cols, 6));
        parser.advance(b"\x1b[1;1Hguard\x1b[2;4r\x1b[4;1Hinside");
        parser.resize(requested_cols, 6);
        assert_eq!(parser.performer.effective_scroll_region(), (1, 3));
        parser.advance(b"\r\n");
        assert_eq!(parser.grid().scrollback_len(), 0);
        assert!(row_text(&parser, 0).starts_with("guard"));
        assert!(row_text(&parser, 2).starts_with("inside"));
    }
}

#[test]
fn parser_resize_does_not_resurrect_margins_or_reset_other_state() {
    // Width changes also normalize margins; resizing cannot erase protocol state or a partial escape.
    let mut parser = Parser::new(Grid::new(12, 6));
    parser.advance(b"\x1b[2;5r\x1b=\x1b[>4;2m\x1b[?1h\x1b[?2004h\x1b[31m");
    let modes = parser.keyboard_modes();
    for (cols, rows) in [(12, 4), (12, 6), (10, 6)] {
        parser.resize(cols, rows);
        assert_eq!(parser.performer.effective_scroll_region(), (0, rows - 1));
        assert_eq!(parser.keyboard_modes(), modes);
        assert!(parser.bracketed_paste_enabled());
        parser.advance(format!("\x1b[{rows};1HX\r\n").as_bytes());
        assert_eq!(parser.grid().row(rows - 2)[0].fg, Color::Indexed(1));
    }
    assert_eq!(parser.grid().scrollback_len(), 3);
    parser.advance(b"\x1b[2;");
    parser.resize(11, 6);
    parser.advance(b"4r");
    assert_eq!(parser.performer.effective_scroll_region(), (1, 3));
}

#[test]
fn parser_resize_keeps_primary_history_and_alternate_scroll_semantics() {
    // Both screens advance visibly after resize, but only primary-screen scrolling grows history.
    let mut parser = Parser::new(Grid::new(12, 6));
    parser.advance(b"\x1b[6;1Hsaved\r\n");
    assert_eq!(parser.grid().scrollback_len(), 1);
    parser.advance(b"\x1b[?1049h\x1b[r");
    parser.resize(12, 4);
    parser.advance(b"\x1b[4;1Halt\r\n");
    assert_eq!(row_text(&parser, 2).trim_end(), "alt");
    assert_eq!(parser.grid().scrollback_len(), 0);
    parser.advance(b"\x1b[?1049l\x1b[4;1Hprimary\r\n");
    assert_eq!(parser.grid().scrollback_len(), 2);
    parser.advance(b"\x1b[r");
    parser.resize(12, 3);
    parser.advance(b"\x1b[3;1H123456789012Z");
    assert_eq!(parser.grid().scrollback_len(), 3);
    assert_eq!(row_text(&parser, 1), "123456789012");
    assert!(row_text(&parser, 2).starts_with('Z'));
}
