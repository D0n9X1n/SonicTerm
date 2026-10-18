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

/// The reader reports the displayed frame's bounds and counts every row it serves.
#[test]
fn reader_counts_rows_and_reports_its_frame() {
    let grid = scrolled_grid();
    let frame = FrameRows::new(&grid, 1);
    assert_eq!((frame.view_top(), frame.view_end()), (1, 5));
    assert!(frame.contains(1) && frame.contains(4));
    assert!(!frame.contains(0) && !frame.contains(5));
    assert!(frame.row(2).is_some());
    assert!(frame.row(2).is_some());
    assert_eq!(frame.rows_read(), 2, "a repeated read counts each time");
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
