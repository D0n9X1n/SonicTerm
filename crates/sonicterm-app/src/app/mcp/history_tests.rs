//! Pins `search_pane`: matches come newest first, a capped call resumes without skipping or
//! repeating, a line straddling two calls is examined once and whole, a line longer than one call
//! is matched on its copied part and marked cut, every call makes progress, and busy, evicted and
//! other-screen cursors are answered without matching.

use regex::Regex;
use serde_json::json;

use super::*;
use crate::app::mcp::read::MAX_CELLS_PER_READ;
use crate::app::mcp::test_entry;

/// Feed `bytes` to `entry`'s parser.
fn emit(entry: &PaneEntry, bytes: &[u8]) {
    entry.parser.lock().advance(bytes);
}

/// Every call from the newest line back until `scan_complete`, bounded so a call that fails to
/// make progress fails the test instead of hanging it.
fn every_call(entry: &PaneEntry, pattern: &str, max_results: usize) -> Vec<SearchPage> {
    let regex = Regex::new(pattern).unwrap();
    let mut pages = Vec::new();
    let mut before = None;
    for _ in 0..1_000 {
        let page = search_history(&entry.parser, &regex, max_results, before);
        assert!(!page.busy && page.refused.is_none());
        let done = page.scan_complete;
        before = page.next_before_row;
        pages.push(page);
        if done {
            // When: the call examined the oldest retained row (`done`), the search is over.
            return pages;
        }
        assert!(before.is_some(), "an incomplete call names where the next one ends");
    }
    panic!("the search never reached the oldest retained row");
}

/// The matched lines of every call, in the order the calls returned them.
fn matched(pages: &[SearchPage]) -> Vec<String> {
    pages.iter().flat_map(|page| page.matches.iter().map(|found| found.line.clone())).collect()
}

/// Matches come newest first, and a call that reaches the oldest row says the scan is complete.
#[test]
fn matches_come_newest_first() {
    let entry = test_entry(20, 5, 100);
    emit(&entry, b"ok 0\r\nhit 1\r\nok 2\r\nhit 3\r\n$ ");
    let pages = every_call(&entry, "hit", DEFAULT_RESULTS);
    assert_eq!(pages.len(), 1);
    assert_eq!(matched(&pages), vec!["hit 3", "hit 1"]);
    assert!(pages[0].at_start && pages[0].scan_complete);
    assert!(pages[0].matches.iter().all(|found| !found.cut));
}

/// A call that reaches `max_results` stops there and the next call resumes before the last match,
/// so every match is returned exactly once, newest first, across the calls.
#[test]
fn a_capped_call_resumes_without_skipping_a_match() {
    let entry = test_entry(20, 5, 1_000);
    for index in 0..7 {
        emit(&entry, format!("noise {index}\r\nhit {index}\r\n").as_bytes());
    }
    let pages = every_call(&entry, "hit", 3);
    let counts: Vec<usize> = pages.iter().map(|page| page.matches.len()).collect();
    assert_eq!(counts, vec![3, 3, 1]);
    let expected: Vec<String> = (0..7).rev().map(|index| format!("hit {index}")).collect();
    assert_eq!(matched(&pages), expected);
    assert!(!pages[0].scan_complete);
}

/// A wrapped line whose rows straddle the cell budget of one call is left to the next call and
/// examined there once and whole: a pattern matching its newest row alone is reported once, on
/// the whole line.
#[test]
fn a_line_straddling_two_calls_is_examined_once_and_whole() {
    let cols = 100usize;
    let entry = test_entry(100, 5, 10_000);
    // One call copies this many full rows; the wrapped line's newest row is the last that fits.
    let rows_per_call = MAX_CELLS_PER_READ / cols;
    let wrapped = format!("NEEDLE{}END", "x".repeat(241));
    emit(&entry, format!("first\r\n{wrapped}\r\n").as_bytes());
    for index in 0..rows_per_call - 2 {
        emit(&entry, format!("filler {index}\r\n").as_bytes());
    }
    emit(&entry, b"last");
    let pages = every_call(&entry, "END", DEFAULT_RESULTS);
    assert_eq!(pages.len(), 2, "the first call ends before the wrapped line");
    assert!(pages[0].matches.is_empty());
    assert_eq!(matched(&pages), vec![wrapped]);
    assert!(!pages[1].matches[0].cut);
}

/// A line longer than one call's budget is matched on each part a call copies, each reported cut,
/// and the search still reaches the line's start.
#[test]
fn a_line_longer_than_one_call_is_matched_on_its_parts_cut() {
    let entry = test_entry(100, 5, 10_000);
    let long = format!("HEAD{}TAIL", "x".repeat(39_992));
    emit(&entry, format!("{long}\r\n$ ").as_bytes());
    let pages = every_call(&entry, "HEAD|TAIL", DEFAULT_RESULTS);
    let found: Vec<&SearchMatch> = pages.iter().flat_map(|page| &page.matches).collect();
    assert_eq!(found.len(), 2, "{:?}", matched(&pages));
    assert!(found[0].line.ends_with("TAIL") && found[1].line.starts_with("HEAD"));
    assert!(found.iter().all(|part| part.cut && part.line.len() <= MAX_MATCH_LINE_BYTES));
}

/// A search with no match pages back through every retained row, each call ending before the last,
/// until it reports the scan complete.
#[test]
fn a_search_with_no_match_pages_until_complete() {
    let entry = test_entry(100, 5, 10_000);
    for index in 0..2_000 {
        emit(&entry, format!("line {index}\r\n").as_bytes());
    }
    let pages = every_call(&entry, "NEVER", DEFAULT_RESULTS);
    assert!(pages.len() > 2, "the history needs several calls");
    assert!(matched(&pages).is_empty());
    let cursors: Vec<u64> =
        pages.iter().filter_map(|page| page.next_before_row).map(|row| row.line).collect();
    assert!(cursors.windows(2).all(|pair| pair[1] < pair[0]), "each call ends further back");
}

/// A `before_row` inside a wrapped line searches that line's start, reported cut because the rest
/// of the line lies after it.
#[test]
fn a_cursor_inside_a_wrap_matches_the_start_cut() {
    let entry = test_entry(10, 5, 100);
    emit(&entry, b"first\r\nNEEDLExxxxyyyyyyyyyyzz");
    // Rows: "first", "NEEDLExxxx", "yyyyyyyyyy", "zz"; the search ends before the third.
    let before = entry.parser.lock().grid().row_token(2);
    let regex = Regex::new("NEEDLE").unwrap();
    let page = search_history(&entry.parser, &regex, DEFAULT_RESULTS, Some(before));
    assert_eq!(page.matches.len(), 1);
    assert_eq!(page.matches[0].line, "NEEDLExxxx");
    assert!(page.matches[0].cut);
}

/// A parser held by someone else returns busy at once, with no matches and the caller's cursor,
/// so the next call retries the same range.
#[test]
fn a_busy_parser_keeps_the_cursor() {
    let entry = test_entry(10, 5, 100);
    emit(&entry, b"one\r\ntwo\r\nthree");
    let before = entry.parser.lock().grid().row_token(2);
    let held = std::sync::Arc::clone(&entry.parser);
    let (locked, wait_for_lock) = std::sync::mpsc::channel();
    let (release, wait_for_release) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = held.lock();
        let _ = locked.send(());
        let _ = wait_for_release.recv();
    });
    wait_for_lock.recv().unwrap();
    let regex = Regex::new("one").unwrap();
    let page = search_history(&entry.parser, &regex, DEFAULT_RESULTS, Some(before));
    release.send(()).unwrap();
    holder.join().unwrap();
    assert!(page.busy && page.matches.is_empty() && !page.scan_complete);
    assert_eq!(page.next_before_row, Some(before));
}

/// The tool refuses a call without a regex, and a cursor whose row left history or names the other
/// screen with its stable code.
#[test]
fn the_tool_refuses_bad_arguments_and_cursors() {
    let entry = test_entry(10, 3, 3);
    let refusal = |reply: Value| reply["structuredContent"]["error"].as_str().map(str::to_owned);
    assert_eq!(refusal(search_reply(&entry, &json!({}))).as_deref(), Some("invalid_arguments"));
    let bad_token = json!({ "regex": "x", "before_row": "q1" });
    assert_eq!(refusal(search_reply(&entry, &bad_token)).as_deref(), Some("invalid_arguments"));
    emit(&entry, b"0\r\n1\r\n2\r\n3");
    let oldest = encode_token(entry.parser.lock().grid().row_token(0));
    emit(&entry, b"\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n10");
    let evicted = json!({ "regex": "x", "before_row": oldest });
    assert_eq!(refusal(search_reply(&entry, &evicted)).as_deref(), Some("evicted"));
    let primary = encode_token(entry.parser.lock().grid().row_token(1));
    emit(&entry, b"\x1b[?1049h");
    let other = json!({ "regex": "x", "before_row": primary });
    assert_eq!(refusal(search_reply(&entry, &other)).as_deref(), Some("other_screen"));
}

/// A result names each match's first row as a token, the cursor for the next call and whether
/// the scan reached the oldest retained row.
#[test]
fn a_result_carries_its_rows_and_cursor() {
    let entry = test_entry(20, 5, 100);
    emit(&entry, b"alpha\r\nbeta\r\n$ ");
    let reply = search_reply(&entry, &json!({ "regex": "beta" }));
    let result = &reply["structuredContent"];
    assert_eq!(result["matches"][0]["line"], "beta");
    assert_eq!(result["matches"][0]["row"], encode_token(RowToken { line: 1, alt: false }));
    assert_eq!(result["matches"][0]["cut"], false);
    assert_eq!(result["scan_complete"], true);
    assert_eq!(result["at_start"], true);
    assert!(result["next_before_row"].is_string(), "{result}");
}

/// `max_results` defaults to 20 and is clamped to 1 through 100.
#[test]
fn max_results_is_defaulted_and_clamped() {
    assert_eq!(max_results_arg(&json!({})), DEFAULT_RESULTS);
    assert_eq!(max_results_arg(&json!({ "max_results": 0 })), 1);
    assert_eq!(max_results_arg(&json!({ "max_results": 5 })), 5);
    assert_eq!(max_results_arg(&json!({ "max_results": 10_000 })), MAX_RESULTS);
}

/// A matched line past 1 KiB is shortened to a window holding the match, cut on character
/// boundaries; a short line is returned whole.
#[test]
fn long_match_lines_are_clipped_around_the_match() {
    let text = format!("{}MARK{}", "é".repeat(2_000), "é".repeat(2_000));
    let (clipped, cut) = clip_match(&text, text.find("MARK").unwrap());
    assert!(cut && clipped.len() <= MAX_MATCH_LINE_BYTES);
    assert!(clipped.contains("MARK"), "{clipped}");
    assert_eq!(clip_match("short", 0), ("short".to_owned(), false));
}
