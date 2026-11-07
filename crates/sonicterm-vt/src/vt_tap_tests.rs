//! Pins the output tap: unarmed it records nothing, armed it keeps printed text including text a
//! carriage return overwrote on screen, it reports when its ring dropped text a reader had not
//! read, and the last reader's release frees it.

use super::super::Parser;
use sonicterm_grid::grid::Grid;

/// A parser on a small grid with a private staging pool.
fn parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(20, 4), None, crate::vt::CaptureStagingPool::new())
}

/// Unarmed, nothing is recorded and a read finds no tap; arming starts at the current end, so
/// text printed earlier is not part of the reader's stream.
#[test]
fn an_unarmed_tap_records_nothing_and_arming_starts_now() {
    let mut parser = parser();
    parser.advance(b"before\r\n");
    assert_eq!(parser.output_tap_since(0), None);
    let hold = parser.arm_output_tap();
    assert_eq!(hold.start, 0, "nothing was recorded before arming");
    parser.advance(b"after\n");
    assert_eq!(parser.output_tap_since(hold.start).unwrap().text, "after\n");
}

/// Text a carriage return overwrote is still in the stream, though the screen shows only the
/// last rewrite; both the ASCII fast path and the vte path record.
#[test]
fn text_overwritten_in_place_stays_in_the_stream() {
    let mut parser = parser();
    let hold = parser.arm_output_tap();
    parser.advance("progress 10%\rprogress 99% ✓\n".as_bytes());
    let text = parser.output_tap_since(hold.start).unwrap().text;
    assert_eq!(text, "progress 10%progress 99% ✓\n");
    let row: String = parser.grid().row(0).iter().map(|cell| cell.ch).collect();
    assert!(!row.contains("10%"), "the screen shows only the rewrite: {row:?}");
}

/// When more than the ring holds is printed after a reader's start, the read is marked
/// truncated and begins at the oldest retained character.
#[test]
fn an_overflowing_ring_reports_truncation() {
    let mut parser = parser();
    let hold = parser.arm_output_tap();
    let start = hold.start;
    let chunk = "x".repeat(1000) + "\n";
    for _ in 0..(super::OUTPUT_TAP_CHARS / 1000 + 2) {
        parser.advance(chunk.as_bytes());
    }
    let read = parser.output_tap_since(start).unwrap();
    assert!(read.truncated);
    assert_eq!(read.text.chars().count(), super::OUTPUT_TAP_CHARS);
    assert_eq!(read.end - read.from, super::OUTPUT_TAP_CHARS as u64);
}

/// Two readers share one tap; dropping a hold takes no lock, and the parser frees the ring at its
/// next batch once the last hold is gone.
#[test]
fn the_last_dropped_hold_frees_the_tap_at_the_next_batch() {
    let mut parser = parser();
    let first = parser.arm_output_tap();
    let second = parser.arm_output_tap();
    drop(first);
    parser.advance(b"x");
    assert!(parser.output_tap_since(0).is_some(), "one reader still holds it");
    drop(second);
    assert!(parser.output_tap_since(0).is_some(), "freed only by the parser, at its next batch");
    parser.advance(b"y");
    assert_eq!(parser.output_tap_since(0), None);
}
