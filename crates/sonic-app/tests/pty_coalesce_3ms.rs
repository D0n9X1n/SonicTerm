//! Epic #300 P3 — PTY coalescer behavior at the 3 ms / 128 KB thresholds.
//!
//! The coalescer itself lives inline in the VT thread closure in
//! `crates/sonic-app/src/app/spawn_pane.rs`. Rather than re-plumb that whole
//! `winit`-coupled loop here, this test re-implements the exact
//! decision-rule the production loop uses and verifies the two contract
//! properties Epic #300 P3 promises:
//!
//! 1. A trickle burst (1 KB / 1 ms over many frames) coalesces to one
//!    redraw request per ~3 ms — never per byte.
//! 2. A single fat burst (>= 128 KB) flushes immediately even if 3 ms has
//!    not elapsed yet, so a `cat largefile` reaches the screen sooner.
//!
//! If the spawn_pane.rs constants ever drift away from `COALESCE_MS = 3`
//! or `FLUSH_BYTES = 128 * 1024`, this test stays green — but a code-review
//! grep + the CLAUDE.md §4 land-mine description are the canonical
//! coupling. The constants are also asserted directly via a textual scan
//! at the bottom of this file so a future edit that drops the byte
//! threshold is caught by the test suite.
//!
//! Time-based assertions use generous tolerance windows so that a
//! virtualised CI runner with high scheduler jitter does not flake.

use std::time::{Duration, Instant};

/// Mirror of the production constants from
/// `crates/sonic-app/src/app/spawn_pane.rs` — kept in lockstep manually and
/// double-checked by `production_constants_match_spec` below.
const COALESCE_MS: u64 = 3;
const FLUSH_BYTES: usize = 128 * 1024;

/// A pure decision function isomorphic to the inline coalescer in
/// `spawn_pane.rs`. Returns `true` if a redraw should fire *now* given
/// elapsed-since-last-redraw and pending byte count.
fn should_flush(elapsed: Duration, pending_bytes: usize) -> bool {
    elapsed >= Duration::from_millis(COALESCE_MS) || pending_bytes >= FLUSH_BYTES
}

/// Burst 1 KB / 1 ms for 30 ms — must produce roughly 30 / 3 = ~10 flushes,
/// NEVER 30. Confirms the coalescer keeps a hot per-byte stream amortised.
#[test]
fn trickle_burst_coalesces_to_three_ms_cadence() {
    let mut flushes: u32 = 0;
    let mut last_flush = Instant::now();
    let mut pending: usize = 0;

    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(30) {
        // Simulated 1 KB arrival each iteration.
        pending = pending.saturating_add(1024);
        if should_flush(last_flush.elapsed(), pending) {
            flushes += 1;
            pending = 0;
            last_flush = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    // 30 ms / 3 ms cadence ≈ 10. CI scheduling jitter pushes this around;
    // accept [3, 20]. The upper bound is the critical one — > 20 means we
    // are flushing too often (i.e. the throttle disappeared).
    assert!(
        (3..=20).contains(&flushes),
        "trickle burst produced {flushes} flushes in 30 ms — expected ~10 \
         (3 ms cadence). Outside [3, 20] means coalescing regressed."
    );
}

/// Single 200 KB arrival must trigger a flush immediately, regardless of
/// elapsed time — that's the byte-threshold early-flush half of the rule.
#[test]
fn fat_burst_flushes_immediately_via_byte_threshold() {
    let pending: usize = 200 * 1024;
    let elapsed = Duration::from_micros(50); // well under the 3 ms timer

    assert!(
        should_flush(elapsed, pending),
        "200 KB pending must early-flush even at {elapsed:?} elapsed \
         (FLUSH_BYTES = {FLUSH_BYTES})"
    );
}

/// A small pending count must NOT flush before 3 ms elapses — guards the
/// other direction (a trivial regression where the threshold is too low).
#[test]
fn small_pending_under_three_ms_does_not_flush() {
    assert!(
        !should_flush(Duration::from_micros(500), 4 * 1024),
        "4 KB at 0.5 ms must NOT flush (would defeat coalescing)"
    );
}

/// Exactly at the FLUSH_BYTES boundary, flush fires. Off-by-one guard.
#[test]
fn at_byte_threshold_flushes() {
    assert!(should_flush(Duration::from_micros(10), FLUSH_BYTES));
    assert!(!should_flush(Duration::from_micros(10), FLUSH_BYTES - 1));
}

/// Exactly at the 3 ms boundary, flush fires. Off-by-one guard.
#[test]
fn at_time_threshold_flushes() {
    assert!(should_flush(Duration::from_millis(COALESCE_MS), 0));
    assert!(!should_flush(Duration::from_millis(COALESCE_MS) - Duration::from_micros(1), 0));
}

/// Textual coupling check: ensure `spawn_pane.rs` actually declares the
/// constants this file mirrors. If someone tunes one without the other
/// this test points them at the drift.
#[test]
fn production_constants_match_spec() {
    let src = include_str!("../src/app/spawn_pane.rs");
    assert!(
        src.contains("const COALESCE_MS: u64 = 3;"),
        "spawn_pane.rs no longer declares `const COALESCE_MS: u64 = 3;` — \
         Epic #300 P3 contract broken or constant renamed without \
         updating this test."
    );
    assert!(
        src.contains("const FLUSH_BYTES: usize = 128 * 1024;"),
        "spawn_pane.rs no longer declares `const FLUSH_BYTES: usize = 128 * 1024;` — \
         Epic #300 P3 byte threshold dropped."
    );
}
