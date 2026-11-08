//! Pins the incremental stream scan against the search it replaced: for seeded random chunking on
//! a small ring, with gaps larger than the ring, its first match, offset, line and truncation flag
//! equal a full search of `output_tap_since(start)`, and the text it copies under the lock is the
//! text printed, not the retained ring once per read.

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{CaptureStagingPool, Parser};

use super::{locate, StreamScan, COPIED_UNDER_LOCK};

/// The ring size the differential runs on, small so the chunks overflow it often.
const RING_CHARS: usize = 64;

/// A parser on a small grid with a private staging pool.
fn parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(40, 8), None, CaptureStagingPool::new())
}

/// A deterministic generator, so a failing seed replays exactly.
struct Seeded(u64);

impl Seeded {
    /// The next value below `bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// Pieces the chunks are cut from: plain words, the patterns' targets, line ends and non-ASCII.
const PIECES: &[&str] =
    &["READY", "xREADY ", "BEGIN ", "END", "\n", "\n\n", "é漢字 ", "xxxx", "ab ", "READ", "Y\n"];

/// One random chunk of one to four pieces.
fn chunk(seeded: &mut Seeded) -> String {
    (0..=seeded.below(4)).map(|_| PIECES[seeded.below(PIECES.len() as u64) as usize]).collect()
}

/// Run `pattern` through both searches over one seeded stream, reading after each step of zero
/// to three chunks (a step can overflow the ring), until the first match or 200 steps; at every
/// read the incremental scan and the full search agree on the match and the truncation flag.
fn differential(pattern: &str, seed: u64) {
    let regex = regex::Regex::new(pattern).unwrap();
    let mut seeded = Seeded(seed);
    let mut parser = parser();
    parser.advance(b"before arming\n");
    let hold = parser.arm_output_tap_with_capacity(RING_CHARS);
    let mut scan = StreamScan::new(hold.start);
    for step in 0..200 {
        // Text published after arming and before the first read is part of the stream too.
        let chunks = if step == 0 { 1 } else { seeded.below(4) };
        for _ in 0..chunks {
            parser.advance(chunk(&mut seeded).as_bytes());
        }
        if seeded.below(8) == 0 {
            // A burst larger than the whole ring between two reads.
            parser.advance("z".repeat(RING_CHARS * 2).as_bytes());
        }
        let full = parser.output_tap_since(hold.start).unwrap();
        let expected = locate(&full.text, full.from, &regex);
        let read = scan.read(&parser).unwrap();
        scan.absorb(read);
        let found = scan.find(&regex);
        let context = format!("pattern {pattern:?} seed {seed} step {step}");
        assert_eq!(scan.truncated(), full.truncated, "{context}");
        assert_eq!(scan.end(), full.end, "{context}");
        if expected.is_some() || found.is_some() {
            // When: either search matched, the wait would stop here, so both must agree.
            assert_eq!(found, expected, "{context}");
            return;
        }
    }
}

/// The patterns whose meaning depends on the whole retained text: anchors, word boundaries, the
/// text start, a multi-line span, empty matches, non-ASCII, and one that never matches.
#[test]
fn the_incremental_scan_matches_a_full_search() {
    let patterns = [
        "^READY",
        r"\bREADY",
        r"\AREADY",
        "(?s)BEGIN.*END",
        "x*",
        "(?m)^$",
        "漢字",
        "NEVER",
        "(?m)READY$",
    ];
    for pattern in patterns {
        for seed in 0..40 {
            differential(pattern, seed);
        }
    }
}

/// An empty-matching pattern matches at the first read even when nothing was printed, as a full
/// search of the empty stream does.
#[test]
fn the_first_read_searches_even_an_empty_stream() {
    let mut parser = parser();
    let hold = parser.arm_output_tap();
    let mut scan = StreamScan::new(hold.start);
    let read = scan.read(&parser).unwrap();
    scan.absorb(read);
    let found = scan.find(&regex::Regex::new("x*").unwrap()).unwrap();
    assert_eq!((found.offset, found.line.as_str()), (hold.start, ""));
}

/// A read with nothing new does not search again, so an idle wait costs no regex pass.
#[test]
fn an_unchanged_buffer_is_not_searched_again() {
    let mut parser = parser();
    let hold = parser.arm_output_tap();
    parser.advance(b"waiting\n");
    let mut scan = StreamScan::new(hold.start);
    let never = regex::Regex::new("NEVER").unwrap();
    let read = scan.read(&parser).unwrap();
    scan.absorb(read);
    assert!(scan.find(&never).is_none());
    assert!(!scan.search_due);
    let read = scan.read(&parser).unwrap();
    scan.absorb(read);
    assert!(!scan.search_due, "an idle read left nothing to search");
}

/// Reading after every one of 2,000 small batches copies each printed character once under the
/// lock: the total equals the text printed, where a whole-ring copy per read would be quadratic.
#[test]
fn the_lock_copies_each_character_once() {
    let mut parser = parser();
    let hold = parser.arm_output_tap();
    let mut scan = StreamScan::new(hold.start);
    let never = regex::Regex::new("NEVER").unwrap();
    COPIED_UNDER_LOCK.with(|copied| copied.set(0));
    let mut printed = 0;
    for index in 0..2_000 {
        let line = format!("line {index}\n");
        printed += line.chars().count();
        parser.advance(line.as_bytes());
        let read = scan.read(&parser).unwrap();
        scan.absorb(read);
        assert!(scan.find(&never).is_none());
    }
    assert_eq!(COPIED_UNDER_LOCK.with(std::cell::Cell::get), printed);
}
