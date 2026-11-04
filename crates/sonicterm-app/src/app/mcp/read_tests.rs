//! Pins bounded pane reads: a tail joins soft wraps and skips blank rows under the cursor, caps
//! hold on huge scrollback, a held parser returns busy quickly, and the last command's output is
//! read from its OSC 133 span.

use std::time::Instant;

use super::*;

/// A parser on a private staging pool and a `cols` x `rows` grid with a scrollback limit.
fn parser(cols: u16, rows: u16) -> Arc<Mutex<Parser>> {
    let mut grid = Grid::new(cols, rows);
    grid.set_scrollback_limit(200_000);
    Arc::new(Mutex::new(Parser::new_with_staging_pool(
        grid,
        None,
        sonicterm_vt::vt::CaptureStagingPool::new(),
    )))
}

/// The texts of a read's lines.
fn texts(snapshot: &ReadSnapshot) -> Vec<&str> {
    snapshot.lines.iter().map(|line| line.text.as_str()).collect()
}

/// A tail read joins a soft-wrapped line into one logical line, returns lines oldest first, and
/// leaves out the blank rows below the prompt.
#[test]
fn a_tail_joins_soft_wraps_and_skips_blank_rows() {
    let pane = parser(10, 6);
    pane.lock().advance(b"first\r\n0123456789abcd\r\n$ ");
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert_eq!(texts(&read), vec!["first", "0123456789abcd", "$"]);
    assert!(!read.truncated && !read.busy);
}

/// A screen read keeps one line per visible row.
#[test]
fn a_screen_read_keeps_each_row() {
    let pane = parser(10, 3);
    pane.lock().advance(b"0123456789abcd");
    let read = read_pane(&pane, ReadMode::Screen, ReadLimits::clamped(None, None));
    assert_eq!(texts(&read), vec!["0123456789", "abcd", ""]);
    assert_eq!(read.size, (10, 3));
}

/// On 100,000 lines of scrollback a tail read stays within its line and byte caps and the cell
/// budget, and says it was truncated.
#[test]
fn a_tail_over_huge_scrollback_stays_within_its_caps() {
    let pane = parser(40, 10);
    {
        let mut guard = pane.lock();
        for line in 0..100_000 {
            guard.advance(format!("line {line}\r\n").as_bytes());
        }
    }
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(Some(5_000), Some(1_000)));
    assert!(read.truncated);
    assert!(read.lines.len() <= MAX_LINES);
    let bytes: usize = read.lines.iter().map(|line| line.text.len() + 1).sum();
    assert!(bytes <= 1_000, "{bytes} bytes");
    assert_eq!(read.lines.last().map(|line| line.text.as_str()), Some("line 99999"));
}

/// A parser held by someone else makes the read return busy without waiting for the holder: the
/// lock is held for 2 s, and the read returns long before that. The bound is loose because CI
/// runners stall for tens of milliseconds; an unbounded wait would take the whole 2 s.
#[test]
fn a_held_parser_returns_busy_quickly() {
    let pane = parser(10, 3);
    let held = std::sync::Arc::clone(&pane);
    let (locked, wait_for_lock) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = held.lock();
        let _ = locked.send(());
        std::thread::sleep(Duration::from_secs(2));
    });
    wait_for_lock.recv().unwrap();
    let started = Instant::now();
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, None));
    let elapsed = started.elapsed();
    assert!(read.busy && read.truncated && read.lines.is_empty());
    assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    holder.join().unwrap();
}

/// The last command's output spans its C row to its D row, with its exit code; with no
/// completed command the read says so.
#[test]
fn the_last_command_is_read_from_its_span() {
    let pane = parser(20, 8);
    let empty = read_pane(&pane, ReadMode::LastCommand, ReadLimits::clamped(None, None));
    assert!(empty.no_command);
    pane.lock().advance(
        b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07built\r\nok\r\n\x1b]133;D;2\x07$ ",
    );
    let read = read_pane(&pane, ReadMode::LastCommand, ReadLimits::clamped(None, None));
    assert_eq!(texts(&read), vec!["built", "ok"]);
    assert_eq!(read.exit_code, Some(2));
}

/// The last command is still found while a full-screen program holds the alternate screen,
/// because the record lives on the primary screen.
#[test]
fn the_last_command_is_found_on_the_alternate_screen() {
    let pane = parser(20, 8);
    pane.lock().advance(
        b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a.txt\r\n\x1b]133;D;0\x07$ \x1b[?1049hvim",
    );
    let read = read_pane(&pane, ReadMode::LastCommand, ReadLimits::clamped(None, None));
    assert!(read.alt_screen);
    assert_eq!(texts(&read), vec!["a.txt"]);
    assert_eq!(read.exit_code, Some(0));
}

/// A command whose output has no trailing newline keeps it: the D row is read up to D's column.
#[test]
fn output_on_the_d_row_is_kept() {
    let pane = parser(20, 8);
    pane.lock()
        .advance(b"\x1b]133;A\x07$ \x1b]133;B\x07printf hi\r\n\x1b]133;C\x07hi\x1b]133;D;0\x07$ ");
    let read = read_pane(&pane, ReadMode::LastCommand, ReadLimits::clamped(None, None));
    assert_eq!(texts(&read), vec!["hi"]);
}

/// A soft wrap keeps the spaces at its boundary, and zero-width marks stay on their character.
#[test]
fn wraps_keep_spaces_and_combining_marks() {
    let pane = parser(5, 4);
    pane.lock().advance("ab   cd\r\ne\u{301}".as_bytes());
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert_eq!(texts(&read), vec!["ab   cd", "e\u{301}"]);
}

/// Spaces at a wrap point survive a byte cap that exactly fits the line: the line is returned
/// whole or not at all, never shortened.
#[test]
fn a_tight_cap_never_drops_spaces_at_a_wrap() {
    let pane = parser(5, 4);
    pane.lock().advance(b"ab   cd");
    // "ab   cd" plus its newline is 8 bytes.
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, Some(8)));
    assert_eq!(texts(&read), vec!["ab   cd"]);
    let tight = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, Some(7)));
    assert!(tight.lines.is_empty() && tight.truncated, "{:?}", texts(&tight));
}

/// Blanks inside a wrapped line count against the byte cap before they are copied, so a long run
/// of them under a small cap stops the read instead of being built and then rejected.
#[test]
fn wrapped_blanks_count_before_they_are_copied() {
    let pane = parser(80, 400);
    let line = format!("a{}b", " ".repeat(30_000));
    pane.lock().advance(line.as_bytes());
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, Some(3)));
    assert!(read.lines.is_empty() && read.truncated);
    // With room for it, the same line reads whole, blanks included.
    let whole = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, Some(64 * 1024)));
    assert_eq!(texts(&whole), vec![line.as_str()]);
}

/// A tail over a screen of blank rows visits at most the cell budget and says it was cut short.
#[test]
fn blank_rows_count_against_the_cell_budget() {
    let pane = parser(1000, 100);
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert!(read.lines.is_empty());
    assert!(read.truncated, "100,000 blank cells pass the 32,768-cell budget");
}

/// A command whose C row already left history still returns the output that remains, marked as
/// truncated.
#[test]
fn surviving_output_is_read_after_the_c_row_left_history() {
    let pane = Arc::new(Mutex::new(Parser::new_with_staging_pool(
        {
            let mut grid = Grid::new(10, 2);
            grid.set_scrollback_limit(1);
            grid
        },
        None,
        sonicterm_vt::vt::CaptureStagingPool::new(),
    )));
    pane.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07x\r\n\x1b]133;C\x07one\r\ntwo\r\nthree\r\nfour\x1b]133;D;0\x07");
    let read = read_pane(&pane, ReadMode::LastCommand, ReadLimits::clamped(None, None));
    assert!(read.output_truncated);
    assert!(texts(&read).contains(&"four"), "{:?}", texts(&read));
}

/// Copying stops at the byte budget: a one-byte read of a long wrapped line returns nothing and
/// says it was cut short, rather than copying the line first.
#[test]
fn copying_stops_at_the_byte_budget() {
    let pane = parser(80, 24);
    pane.lock().advance(&vec![b'z'; 1000]);
    let read = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, Some(1)));
    assert!(read.lines.is_empty());
    assert!(read.truncated);
    let copy = row_cells_text_capped(&pane.lock().grid().row_at_abs(0).unwrap().clone(), 80, 5);
    assert_eq!((copy.text.as_str(), copy.cut), ("zzzzz", true));
}

/// Wide characters contribute one character, not their right-half spacer.
#[test]
fn wide_characters_are_read_once() {
    let pane = parser(10, 2);
    pane.lock().advance("日本x".as_bytes());
    let read = read_pane(&pane, ReadMode::Screen, ReadLimits::clamped(None, None));
    assert_eq!(read.lines[0].text, "日本x");
}

/// Caps are clamped: zero becomes one, values past the maximums become the maximums.
#[test]
fn limits_are_clamped() {
    assert_eq!(
        ReadLimits::clamped(None, None),
        ReadLimits { lines: DEFAULT_LINES, bytes: DEFAULT_BYTES }
    );
    assert_eq!(
        ReadLimits::clamped(Some(0), Some(u64::MAX)),
        ReadLimits { lines: 1, bytes: MAX_BYTES }
    );
}
