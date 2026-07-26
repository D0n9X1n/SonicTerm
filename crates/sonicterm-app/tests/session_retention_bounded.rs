//! A long session stays bounded.
//!
//! Every retention seam has its own ceiling and each is unit-tested against
//! it. What no per-seam test can show is what happens when a pane is driven
//! hard across *all* of them at once for a sustained period — which is the
//! only shape in which the reported multi-gigabyte growth was ever observed.
//!
//! These tests drive a pane the way a real session drives it: continuous
//! scrolling output, thousands of distinct hyperlinks, escape sequences,
//! alternate-screen transitions.
//!
//! The seams do not all have the same contract, and asserting one blanket
//! "everything converges" claim over them is wrong:
//!
//! * **Grid** reaches a true steady state. Once scrollback is full, evicting
//!   one row per new row holds it flat forever.
//! * **Hyperlinks** grow monotonically until their cap. Links in retained
//!   scrollback are still reachable by scrolling back to them, so freeing one
//!   early would break a link the user can still see. Growth stops at the cap,
//!   where reclamation makes the cap survivable instead of terminal.
//!
//! Each test below asserts the contract that actually applies to its seam.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_app::app::retention::{measure_pane, measure_panes, PaneRetention};
use sonicterm_app::app::PaneState;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

const SCROLLBACK_ROWS: usize = 5_000;
const LINES_PER_ROUND: usize = 40;
/// Round at which scrollback is full and the grid seam must stop growing.
const PLATEAU_ROUND: usize = SCROLLBACK_ROWS / LINES_PER_ROUND;

/// Roughly one screenful of mixed shell output, repeated to simulate a session.
fn write_session_burst(parser: &mut Parser, round: usize) {
    for line in 0..LINES_PER_ROUND {
        parser.advance(
            format!("[{round:05}:{line:02}] building target/debug/deps/module.rs ... ok\r\n")
                .as_bytes(),
        );
    }
    // Distinct hyperlinks — the seam that used to wedge permanently.
    for link in 0..8 {
        parser.advance(
            format!(
                "\x1b]8;;https://example.com/build/{round}/artifact/{link}\x07artifact\x1b]8;;\x07\r\n"
            )
            .as_bytes(),
        );
    }
    // SGR churn, as a TUI would emit.
    parser.advance(b"\x1b[1;32mgreen\x1b[0m \x1b[4munderline\x1b[24m \x1b[7minverse\x1b[27m\r\n");
}

fn pane() -> PaneState {
    let mut grid = Grid::new(120, 40);
    grid.set_scrollback_limit(SCROLLBACK_ROWS);
    PaneState::new(Arc::new(Mutex::new(Parser::new(grid))), None)
}

fn measure(pane: &PaneState) -> PaneRetention {
    measure_pane(pane).expect("a single-threaded test never contends the parser lock")
}

/// Cells reach a true steady state once scrollback is full.
///
/// This is the seam that dominates a pane's footprint — ~14.5 MiB here against
/// a few hundred KiB for everything else — so it is the one whose flatness
/// determines whether a long session is bounded. Scrollback eviction must hold
/// it exactly level: one row in, one row out.
#[test]
fn the_grid_seam_plateaus_once_scrollback_is_full() {
    let pane = pane();

    for round in 0..PLATEAU_ROUND {
        write_session_burst(&mut pane.parser.lock(), round);
    }
    let at_plateau = measure(&pane).grid.bytes;

    // Three times as much work again after the plateau.
    for round in PLATEAU_ROUND..PLATEAU_ROUND * 4 {
        write_session_burst(&mut pane.parser.lock(), round);
    }
    let after = measure(&pane).grid.bytes;

    assert_eq!(
        after, at_plateau,
        "the grid seam must stay exactly flat once scrollback is full: \
         {at_plateau} at round {PLATEAU_ROUND}, {after} after three times more work"
    );
    assert_eq!(
        pane.parser.lock().grid().scrollback_len(),
        SCROLLBACK_ROWS,
        "precondition: scrollback is actually full"
    );
}

/// A long session's total stays inside a sane envelope for one pane.
///
/// The grid plateaus and the hyperlink seam creeps toward its cap, so the
/// total is bounded even though not every seam is flat. This pins the
/// magnitude: a regression that reintroduced unbounded retention in any seam
/// would blow through this envelope regardless of which seam it was.
#[test]
fn a_long_session_stays_within_a_one_pane_envelope() {
    let pane = pane();
    const ROUNDS: usize = 600;

    for round in 0..ROUNDS {
        write_session_burst(&mut pane.parser.lock(), round);
    }

    let retention = measure(&pane);
    let total = retention.total().bytes;

    // 120x40 visible + 5k scrollback at 24 bytes/cell is ~14.7 MiB of cells.
    // The envelope allows generous headroom over that without admitting a leak.
    const ONE_PANE_ENVELOPE: usize = 64 * 1024 * 1024;
    assert!(
        total < ONE_PANE_ENVELOPE,
        "one pane retained {total} bytes after {ROUNDS} rounds, above the \
         {ONE_PANE_ENVELOPE}-byte envelope (grid {}, parser {}, links {}, media {})",
        retention.grid.bytes,
        retention.parser.bytes,
        retention.hyperlinks.bytes,
        retention.inline_media.bytes
    );

    // The grid must be the dominant term. If something else overtakes it, the
    // shape of the session has changed and this test's envelope no longer
    // describes what it claims to.
    let (seam, _) = retention.largest_seam();
    assert_eq!(seam, "grid", "cells must dominate a scrolling session's retention");
}

/// In-flight parser buffers do not survive the sequences that created them.
///
/// Escape and media captures are transient by contract: they exist while a
/// sequence is being parsed and are released when it completes. A capture that
/// outlived its sequence would accumulate once per escape, which a
/// TUI-heavy session emits thousands of times a minute.
#[test]
fn parser_capture_does_not_survive_completed_sequences() {
    let pane = pane();

    {
        let mut parser = pane.parser.lock();
        for round in 0..200 {
            // A completed media sequence, the largest transient capture.
            parser.advance(b"\x1bPq#0;2;0;0;0#0~~~~~\x1b\\");
            // Completed OSC and CSI sequences.
            parser.advance(format!("\x1b]0;title {round}\x07").as_bytes());
            parser.advance(b"\x1b[1;2;3;4;5;6;7;8;9;10m\x1b[0m");
        }
    }

    let retention = measure(&pane);
    assert_eq!(
        retention.parser.bytes, 0,
        "completed sequences must leave no capture behind, found {} bytes",
        retention.parser.bytes
    );
}

/// Hyperlinks keep working for the whole session.
///
/// The registry caps distinct links at 16,384. A session emitting more than
/// that used to stop rendering links permanently — `intern` returned the
/// reserved invalid id and nothing ever recovered. This drives well past the
/// cap and asserts links still resolve at the end, which is what the user
/// sees, while the registry stays inside its own cap.
#[test]
fn links_still_resolve_after_far_more_than_the_registry_cap() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let pane = pane();
    let emitted = MAX_HYPERLINKS * 2;
    {
        let mut parser = pane.parser.lock();
        for index in 0..emitted {
            parser.advance(
                format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
            );
        }
        parser.advance(b"\x1b]8;;https://example.com/last\x07final\x1b]8;;\x07");
    }

    let parser = pane.parser.lock();
    let row = parser.grid().cursor.row;
    let hid = parser
        .grid()
        .row(row)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the final link must reach a cell");
    assert_eq!(
        parser.hyperlinks().lookup(hid).map(|link| link.uri.as_str()),
        Some("https://example.com/last"),
        "after {emitted} links the newest must still resolve"
    );
    assert!(
        parser.hyperlinks().len() <= MAX_HYPERLINKS,
        "the registry must stay within its own cap while remaining usable"
    );
}

/// Alternate-screen cycling does not accumulate.
///
/// Entering the alt screen saves the primary; leaving restores it. A session
/// that runs full-screen programs cycles this repeatedly, so any
/// per-transition retention would compound over a working day.
#[test]
fn repeated_alt_screen_cycles_do_not_accumulate() {
    let pane = pane();

    write_session_burst(&mut pane.parser.lock(), 0);
    let baseline = measure(&pane).total().bytes;

    {
        let mut parser = pane.parser.lock();
        for cycle in 0..200 {
            parser.advance(b"\x1b[?1049h");
            parser.advance(format!("full-screen program frame {cycle}\r\n").as_bytes());
            parser.advance(b"\x1b[?1049l");
        }
    }

    let after = measure(&pane).total().bytes;
    assert!(
        after <= baseline * 2,
        "200 alt-screen cycles must not compound retention: {baseline} → {after}"
    );
}

/// The session total is the sum over panes, and each pane stays compliant.
///
/// This is the composition that produced the field reports: every pane inside
/// its own ceiling while the process total is their sum. The test pins both
/// halves so a future per-pane change cannot silently move the aggregate.
#[test]
fn a_multi_pane_session_totals_its_compliant_panes() {
    const PANES: usize = 12;
    let panes: Vec<PaneState> = (0..PANES).map(|_| pane()).collect();

    for (index, pane) in panes.iter().enumerate() {
        let mut parser = pane.parser.lock();
        for round in 0..PLATEAU_ROUND {
            write_session_burst(&mut parser, round + index * PLATEAU_ROUND);
        }
    }

    let per_pane: Vec<usize> = panes.iter().map(|pane| measure(pane).total().bytes).collect();
    let aggregate = measure_panes(panes.iter()).total().bytes;

    const ONE_PANE_ENVELOPE: usize = 64 * 1024 * 1024;
    for (index, bytes) in per_pane.iter().enumerate() {
        assert!(
            *bytes < ONE_PANE_ENVELOPE,
            "pane {index} retained {bytes} bytes, above the per-pane envelope"
        );
    }

    let expected: usize = per_pane.iter().sum();
    assert_eq!(aggregate, expected, "the aggregate must equal the sum of its panes");
    assert!(
        aggregate > per_pane[0],
        "the aggregate must exceed any single pane: {aggregate} vs {}",
        per_pane[0]
    );
}
