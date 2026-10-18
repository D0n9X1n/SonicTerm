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
    assert_eq!(frame.read_span(), Some((2, 2)), "a refused row never widens the read span");
    assert!(frame.row(4).is_some());
    assert_eq!(frame.read_span(), Some((2, 4)));
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

/// Production sources of path detection other than the reader itself, by file name, so the
/// source check covers each and can tell when a new module joins the directory unlisted.
const DETECTION_SOURCES: &[(&str, &str)] = &[
    ("path_target.rs", include_str!("../path_target.rs")),
    ("hover_cache.rs", include_str!("hover_cache.rs")),
    ("linux.rs", include_str!("linux.rs")),
    ("macos.rs", include_str!("macos.rs")),
    ("unix.rs", include_str!("unix.rs")),
    ("windows.rs", include_str!("windows.rs")),
];

/// `line` without its line comment: a `//` at the start, or one set off by spaces after code,
/// which a `scheme://` inside a string literal never is.
fn code_part(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        // When: the whole line is a comment, none of it is code.
        return "";
    }
    line.find(" // ").map_or(line, |comment_start| &line[..comment_start])
}

/// Detection reads rows only through `FrameRows`: `FrameRows::grid` still hands out the grid for
/// its geometry and screen identity, so this source check pins that no detection source other
/// than the reader calls `row_at_abs`, or `.row(` on a grid, outside comments, where it could read
/// a row the pane does not display.
#[test]
fn path_detection_never_reads_rows_past_the_frame_reader() {
    let mut direct_reads = Vec::new();
    for (file_name, source) in DETECTION_SOURCES {
        for (index, line) in source.lines().enumerate() {
            let code = code_part(line);
            if code.contains("row_at_abs")
                || code.contains("grid().row(")
                || code.contains("grid.row(")
            {
                // When: the code part reads a grid row directly, report its file and line.
                direct_reads.push(format!("{file_name}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert!(direct_reads.is_empty(), "detection reads rows directly: {direct_reads:?}");
}

/// Every production source in `path_target/` except the reader is in `DETECTION_SOURCES`, so a
/// new detection module cannot read rows directly unchecked.
#[test]
fn the_frame_reader_source_check_covers_every_detection_module() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/path_target");
    let mut on_disk = std::fs::read_dir(&directory)
        .expect("path_target directory")
        .map(|entry| entry.expect("directory entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && !name.ends_with("_tests.rs"))
        .filter(|name| name != "frame_rows.rs")
        .collect::<Vec<_>>();
    on_disk.sort();
    let mut listed = DETECTION_SOURCES
        .iter()
        .map(|(file_name, _)| file_name.to_string())
        .filter(|name| name != "path_target.rs")
        .collect::<Vec<_>>();
    listed.sort();
    assert_eq!(listed, on_disk);
}
