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

/// The tokens of `code` with their byte offsets, skipping whitespace: a word (identifier or
/// number) is one token and every other char is its own, so a match ignores spacing and line breaks.
fn code_tokens(code: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut chars = code.char_indices().peekable();
    while let Some((start, current)) = chars.next() {
        if current.is_whitespace() {
            // When: `current` is whitespace, it separates tokens and is none itself.
            continue;
        }
        let mut end = start + current.len_utf8();
        if current.is_alphanumeric() || current == '_' {
            // When: `current` starts a word, the token runs to the end of the word.
            while let Some(&(next_start, next)) = chars.peek() {
                if !(next.is_alphanumeric() || next == '_') {
                    // When: `next` cannot continue the word, the word token ends before it.
                    break;
                }
                end = next_start + next.len_utf8();
                chars.next();
            }
        }
        found.push((start, &code[start..end]));
    }
    found
}

/// Byte offsets in `code` where the token sequence of `pattern` starts. Whole tokens compare, so
/// `grid.row(` matches `frame.grid\n    .row(0)` but not `grid.rows(`.
fn token_matches(code: &str, pattern: &str) -> Vec<usize> {
    let wanted = code_tokens(pattern);
    let present = code_tokens(code);
    if wanted.is_empty() {
        // When: `pattern` has no token, it can match nothing.
        return Vec::new();
    }
    present
        .windows(wanted.len())
        .filter(|window| window.iter().zip(&wanted).all(|(have, want)| have.1 == want.1))
        .map(|window| window[0].0)
        .collect()
}

/// The 1-based line of byte `offset` in `text`.
fn line_number(text: &str, offset: usize) -> usize {
    1 + text.as_bytes()[..offset.min(text.len())].iter().filter(|byte| **byte == b'\n').count()
}

/// Token sequences that read a grid row directly: the absolute-row accessor, and `.row(` on a grid
/// binding or on the grid `FrameRows::grid` hands out.
const DIRECT_ROW_READS: [&str; 3] = ["row_at_abs", "grid().row(", "grid.row("];

/// Each direct row read in `source`, as its 1-based line and the pattern it matched. Comments and
/// literal contents are blanked first (the crate's shared `code_views` scanner), and tokens match
/// across spaces and line breaks.
fn direct_row_reads(source: &str) -> Vec<(usize, &'static str)> {
    let (_, code) = crate::app::source_scan_support::code_views(source);
    let mut reads = DIRECT_ROW_READS
        .iter()
        .flat_map(|pattern| {
            token_matches(&code, pattern)
                .into_iter()
                .map(|offset| (line_number(&code, offset), *pattern))
        })
        .collect::<Vec<_>>();
    reads.sort_unstable();
    reads
}

/// A row accessor named only in a block comment, a line comment or a string, including a URL
/// holding `//`, is no read, and a read through the frame reader is allowed.
#[test]
fn direct_row_read_scan_ignores_comments_literals_and_frame_reads() {
    let source = concat!(
        "/* grid.row(0) */ let name = \"row_at_abs\"; // frame.grid().row(1)\n",
        "let url = \"https://grid.row(2)\"; let raw = r#\"parser.grid().row(3)\"#;\n",
        "let kept = frame.row(4);\n",
    );
    assert_eq!(direct_row_reads(source), Vec::new());
}

/// A real read is found after a string holding ` // `, when the call is split across lines and when
/// it reaches the absolute accessor, each on the line where its receiver starts.
#[test]
fn direct_row_read_scan_finds_reads_after_literals_and_across_lines() {
    let source = concat!(
        "let note = \" // \"; let row = grid.row(0);\n",
        "let split = frame.grid()\n",
        "    .row(1);\n",
        "let abs = parser.grid().row_at_abs(5); let allowed = frame.row(2);\n",
    );
    assert_eq!(
        direct_row_reads(source),
        vec![(1, "grid.row("), (2, "grid().row("), (4, "row_at_abs")]
    );
}

/// Detection reads rows only through `FrameRows`: `FrameRows::grid` still hands out the grid for
/// its geometry and screen identity, so this source check pins that no detection source other
/// than the reader calls `row_at_abs`, or `.row(` on a grid, in code, where it could read a row
/// the pane does not display.
#[test]
fn path_detection_never_reads_rows_past_the_frame_reader() {
    let direct_reads = DETECTION_SOURCES
        .iter()
        .flat_map(|(file_name, source)| {
            direct_row_reads(source)
                .into_iter()
                .map(move |(line, pattern)| format!("{file_name}:{line}: {pattern}"))
        })
        .collect::<Vec<_>>();
    assert!(direct_reads.is_empty(), "detection reads rows directly: {direct_reads:?}");
}

/// Every production Rust source under `directory`, at any depth, as a `/`-separated path relative
/// to it, sorted; test siblings (`*_tests.rs`) and the reader `frame_rows.rs` are left out, so a
/// nested module such as `helper/mod.rs` is listed by its relative path.
fn production_modules_under(directory: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).expect("readable source directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                // When: `path` is a subdirectory, its modules are walked too, so none escapes.
                pending.push(path);
                continue;
            }
            let relative = path.strip_prefix(directory).expect("walked path is under the root");
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if relative.ends_with(".rs")
                && !relative.ends_with("_tests.rs")
                && relative != "frame_rows.rs"
            {
                // When: `relative` is a production source other than the reader, it must be listed.
                found.push(relative);
            }
        }
    }
    found.sort();
    found
}

/// Every production source in `path_target/`, at any depth, except the reader is in
/// `DETECTION_SOURCES`, so a new detection module, flat or nested, cannot read rows unchecked.
#[test]
fn the_frame_reader_source_check_covers_every_detection_module() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/path_target");
    let mut listed = DETECTION_SOURCES
        .iter()
        .map(|(file_name, _)| file_name.to_string())
        .filter(|name| name != "path_target.rs")
        .collect::<Vec<_>>();
    listed.sort();
    assert_eq!(listed, production_modules_under(&directory));
}

/// A nested module such as `helper/mod.rs` is found by its relative path, so it cannot escape the
/// coverage check that a flat listing would pass; test siblings at any depth and the reader are
/// skipped, and a non-Rust file is ignored. The fixture tree is built in a private temporary
/// directory and removed before asserting, so a failure leaves nothing behind.
#[test]
fn a_nested_detection_module_cannot_escape_the_coverage_check() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let root = std::env::temp_dir()
        .join(format!("sonicterm-frame-rows-guard-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(root.join("helper/deeper")).expect("fixture directories");
    for file_name in [
        "unix.rs",
        "unix_tests.rs",
        "frame_rows.rs",
        "notes.txt",
        "helper/mod.rs",
        "helper/mod_tests.rs",
        "helper/deeper/scan.rs",
    ] {
        std::fs::write(root.join(file_name), "").expect("fixture file");
    }
    let found = production_modules_under(&root);
    std::fs::remove_dir_all(&root).expect("fixture cleanup");
    assert_eq!(found, vec!["helper/deeper/scan.rs", "helper/mod.rs", "unix.rs"]);
    assert_ne!(found, vec!["unix.rs"], "a flat listing would let the nested modules through");
}
