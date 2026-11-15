//! Pins paging back through history: pages from the newest line reach the oldest retained row and
//! return each line once, a wrapped line is never split across pages unless it alone outgrows a
//! page, every page makes progress under tiny caps, and tokens that left history or name the other
//! screen are refused.

use super::*;

/// A parser on a private staging pool and a `cols` x `rows` grid keeping `scrollback` lines.
fn parser(cols: u16, rows: u16, scrollback: usize) -> Arc<Mutex<Parser>> {
    let mut grid = Grid::new(cols, rows);
    grid.set_scrollback_limit(scrollback);
    Arc::new(Mutex::new(Parser::new_with_staging_pool(
        grid,
        None,
        sonicterm_vt::vt::CaptureStagingPool::new(),
    )))
}

/// One history page before `before` under `lines` and `bytes` caps.
fn page(
    pane: &Arc<Mutex<Parser>>,
    before: Option<RowToken>,
    lines: u64,
    bytes: u64,
) -> ReadSnapshot {
    let limits = ReadLimits::clamped(Some(lines), Some(bytes));
    read_pane(pane, ReadMode::History { before }, limits)
}

/// Every page from the newest line back to the start, newest page first. Bounded so a page that
/// fails to make progress fails the test instead of hanging it.
fn every_page(pane: &Arc<Mutex<Parser>>, lines: u64, bytes: u64) -> Vec<ReadSnapshot> {
    let mut pages = Vec::new();
    let mut before = None;
    for _ in 0..10_000 {
        let read = page(pane, before, lines, bytes);
        assert!(read.refused.is_none() && !read.busy, "{:?}", read.refused);
        let done = read.at_start;
        before = read.first_row;
        pages.push(read);
        if done {
            // When: the page reached the oldest retained row (`done`), paging ends.
            return pages;
        }
        assert!(before.is_some(), "a page short of the start names where the next one ends");
    }
    panic!("paging never reached the oldest retained row");
}

/// The texts of a page's lines.
fn texts(read: &ReadSnapshot) -> Vec<&str> {
    read.lines.iter().map(|line| line.text.as_str()).collect()
}

/// Paging from the newest line with a small line cap returns every retained line exactly once and
/// in order, and the last page says it reached the start.
#[test]
fn paging_history_returns_every_line_once() {
    let pane = parser(20, 5, 10_000);
    {
        let mut guard = pane.lock();
        for line in 0..500 {
            guard.advance(format!("line {line}\r\n").as_bytes());
        }
    }
    let pages = every_page(&pane, 37, 64 * 1024);
    let mut seen: Vec<String> = Vec::new();
    // Pages arrive newest first and each holds its lines oldest first.
    for read in pages.iter().rev() {
        seen.extend(read.lines.iter().map(|line| line.text.clone()));
    }
    let expected: Vec<String> = (0..500).map(|line| format!("line {line}")).collect();
    assert_eq!(seen, expected);
    assert!(pages.last().unwrap().at_start);
    assert!(pages.iter().all(|read| read.lines.len() <= 37));
}

/// A soft-wrapped line that does not fit what is left of a page is left out whole and returned
/// whole at the start of the next page, never split between the two.
#[test]
fn a_wrapped_line_across_a_page_boundary_is_returned_once_and_whole() {
    let pane = parser(10, 3, 1_000);
    let wrapped = "W".repeat(35);
    pane.lock().advance(format!("a\r\nb\r\n{wrapped}\r\nc\r\nd").as_bytes());
    // "c" and "d" cost 2 bytes each; the 36-byte wrapped line would pass the 39-byte cap.
    let pages = every_page(&pane, 1_000, 39);
    let page_texts: Vec<Vec<&str>> = pages.iter().map(texts).collect();
    assert_eq!(page_texts, vec![vec!["c", "d"], vec!["b", wrapped.as_str()], vec!["a"]]);
    assert!(pages.iter().flat_map(|read| &read.lines).all(|line| !line.cut));
}

/// A wrapped line longer than a whole page is returned in parts, each marked cut, whose texts
/// joined oldest first rebuild the line exactly.
#[test]
fn a_line_longer_than_the_page_budget_pages_through_with_cut() {
    let pane = parser(10, 3, 1_000);
    let long = "x".repeat(300);
    pane.lock().advance(format!("{long}\r\nend").as_bytes());
    let pages = every_page(&pane, 1_000, 64);
    assert_eq!(texts(&pages[0]), vec!["end"]);
    let parts = &pages[1..];
    assert!(parts.len() > 1, "the line needs several pages");
    for read in parts {
        assert_eq!(read.lines.len(), 1, "{:?}", texts(read));
        assert!(read.lines[0].cut, "every part of the long line is marked cut");
    }
    let rebuilt: String = parts.iter().rev().map(|read| read.lines[0].text.as_str()).collect();
    assert_eq!(rebuilt, long);
}

/// A byte cap that fits only one short line returns one whole line per page; a one-byte cap
/// returns one empty, cut line per row and still reaches the start.
#[test]
fn a_tiny_byte_cap_still_makes_progress() {
    let pane = parser(10, 3, 1_000);
    pane.lock().advance(b"ab\r\ncd\r\nef");
    let pages = every_page(&pane, 1_000, 3);
    let page_texts: Vec<Vec<&str>> = pages.iter().map(texts).collect();
    assert_eq!(page_texts, vec![vec!["ef"], vec!["cd"], vec!["ab"]]);
    let pages = every_page(&pane, 1_000, 1);
    assert_eq!(pages.len(), 3, "one row per page");
    for read in &pages {
        assert_eq!(read.lines.len(), 1);
        assert!(read.lines[0].cut && read.lines[0].text.is_empty());
    }
}

/// A token whose row left history is refused as evicted, so an agent restarts from the newest
/// line instead of silently reading a different row.
#[test]
fn an_evicted_token_is_refused() {
    let pane = parser(10, 3, 5);
    pane.lock().advance(b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6");
    let oldest = page(&pane, None, 1_000, 64 * 1024);
    let token = oldest.first_row.expect("the read covered rows");
    pane.lock().advance(b"\r\n7\r\n8\r\n9\r\n10\r\n11\r\n12\r\n13\r\n14");
    let read = page(&pane, Some(token), 1_000, 64 * 1024);
    assert_eq!(read.refused, Some(HistoryRefusal::Evicted));
    assert!(read.lines.is_empty());
}

/// A primary-screen token is refused while the alternate screen is active, rather than read as a
/// row of a different screen.
#[test]
fn a_token_from_the_other_screen_is_refused() {
    let pane = parser(10, 3, 100);
    pane.lock().advance(b"one\r\ntwo");
    let token = page(&pane, None, 1_000, 64 * 1024).first_row.unwrap();
    pane.lock().advance(b"\x1b[?1049hfull screen");
    let read = page(&pane, Some(token), 1_000, 64 * 1024);
    assert_eq!(read.refused, Some(HistoryRefusal::OtherScreen));
    assert_eq!(HistoryRefusal::OtherScreen.as_str(), "other_screen");
    assert_eq!(HistoryRefusal::Evicted.as_str(), "evicted");
}

/// A page that ends just before a wrapped line's later rows returns the line's start as a cut
/// fragment, since the rest of it lies after the page.
#[test]
fn a_page_ending_inside_a_wrapped_line_marks_it_cut() {
    let pane = parser(10, 5, 100);
    pane.lock().advance(b"first\r\nNEEDLExxxxyyyyyyyyyyzz");
    let start = pane.lock().grid().scrollback_len() as u64;
    // Rows: "first", "NEEDLExxxx", "yyyyyyyyyy", "zz"; the page ends before the third.
    let before = pane.lock().grid().row_token(start + 2);
    let read = page(&pane, Some(before), 1_000, 64 * 1024);
    assert_eq!(texts(&read), vec!["first", "NEEDLExxxx"]);
    assert_eq!(read.lines.iter().map(|line| line.cut).collect::<Vec<_>>(), vec![false, true]);
}

/// Every read reports the first row of its oldest returned line and whether that is the oldest
/// retained row: a tail of a short pane reaches the start, a capped tail does not, and an empty
/// page before the oldest row reports no row and the start.
#[test]
fn every_read_reports_its_first_row_and_whether_it_reached_the_start() {
    let pane = parser(10, 3, 100);
    pane.lock().advance(b"a\r\nb\r\nc\r\nd\r\ne");
    let whole = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert!(whole.at_start);
    assert_eq!(whole.first_row, Some(whole.lines[0].first_row));
    let capped = read_pane(&pane, ReadMode::Tail, ReadLimits::clamped(Some(2), None));
    assert_eq!(texts(&capped), vec!["d", "e"]);
    assert!(!capped.at_start);
    assert_eq!(capped.first_row, Some(capped.lines[0].first_row));
    let oldest = pane.lock().grid().row_token(0);
    let empty = page(&pane, Some(oldest), 1_000, 64 * 1024);
    assert!(empty.lines.is_empty() && empty.at_start && empty.first_row.is_none());
}
