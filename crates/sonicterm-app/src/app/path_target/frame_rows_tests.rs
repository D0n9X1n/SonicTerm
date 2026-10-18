use super::*;

/// Scroll a 4-row grid back by two rows, so rows 0..2 are history and the frame is rows 0..4.
fn scrolled_grid() -> Grid {
    let mut grid = Grid::new(10, 4);
    for line in ["one", "two", "three", "four", "five", "six"] {
        for character in line.chars() {
            grid.put_char(
                character,
                sonicterm_grid::grid::Color::Default,
                sonicterm_grid::grid::Color::Default,
                sonicterm_grid::grid::CellFlags::empty(),
            );
        }
        grid.carriage_return();
        grid.linefeed();
    }
    grid
}

/// The reader reports the displayed frame's bounds, serves only displayed rows and counts every
/// row it serves; a refused row outside the frame is not counted as read.
#[test]
fn reader_serves_and_counts_only_displayed_rows() {
    let grid = scrolled_grid();
    let frame = FrameRows::new(&grid, 1);
    assert_eq!((frame.view_top(), frame.view_end()), (1, 5));
    assert!(frame.contains(1) && frame.contains(4));
    assert!(!frame.contains(0) && !frame.contains(5));
    assert!(frame.row(2).is_some());
    assert!(frame.row(2).is_some());
    assert!(frame.row(0).is_none() && frame.row(5).is_none(), "rows outside the frame are refused");
    assert_eq!(frame.rows_read(), 2, "a repeated read counts each time; a refused one never");
}

/// `rows_below` is true exactly when the view is scrolled back, read from the row count alone.
#[test]
fn rows_below_reports_a_scrolled_back_view() {
    let grid = scrolled_grid();
    let live_top = grid.scrollback_len() as u64;
    assert!(FrameRows::new(&grid, live_top - 1).rows_below());
    assert!(!FrameRows::new(&grid, live_top).rows_below());
    let ((), reads) = read_observer::observe(|| {
        let _ = FrameRows::new(&grid, 0).rows_below();
    });
    assert!(reads.is_empty(), "the answer reads no row");
}

/// The test observer records every requested row with whether the frame displayed it, so a
/// detection test can prove no read left the frame.
#[test]
fn observer_records_reads_inside_and_outside_the_frame() {
    let grid = scrolled_grid();
    let frame = FrameRows::new(&grid, 1);
    let ((), reads) = read_observer::observe(|| {
        let _ = frame.row(1);
        let _ = frame.row(5);
    });
    assert_eq!(reads, vec![(1, true), (5, false)]);
    let ((), outside) = read_observer::observe(|| ());
    assert!(outside.is_empty(), "an observation sees only its own reads");
}
