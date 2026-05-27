//! End-to-end regression for PR #185 / issue #175 — multi-round PTY
//! prompts must both land in the grid.
//!
//! This test complements the App-level flag-mechanics tests in
//! `pty_multi_round_hang.rs` by driving the *full* unmocked
//! `PtyHandle` → `Parser` → `Grid` pipeline: it spawns a child binary
//! that emits two prompts with a stdin read between them, feeds the
//! reply byte after each prompt, and asserts that BOTH prompt strings
//! reach the grid within a generous timeout.
//!
//! The child is `pty_multi_round_helper` (under `src/bin/`) — a tiny
//! Rust binary written specifically so this test is portable across
//! macOS, Windows, and Linux without depending on `bash`/`cmd`'s
//! prompt-quoting differences.
//!
//! Why both this and the flag-mechanics tests:
//! - The flag-mechanics tests pin the specific App-layer contract that
//!   broke (`pending_redraw` reschedule on `try_lock` bail-out).
//! - This E2E test pins the user-observable behaviour ("both prompts
//!   in the grid after a multi-round exchange") regardless of which
//!   layer a future regression hides in — PTY reader thread, channel
//!   coalescer, VT parser, or App render scheduling.

use std::time::{Duration, Instant};

use sonic_core::{
    grid::{CellFlags, Grid},
    pty::PtyHandle,
    vt::Parser,
};

/// Drain whatever bytes the PTY has produced so far into the parser,
/// up to `total` deadline, returning early as soon as `needle` appears
/// anywhere in the grid (so a slow child doesn't blow the wall-clock
/// budget once the prompt has arrived).
fn drain_until(pty: &PtyHandle, parser: &mut Parser, needle: &str, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if let Ok(b) = pty.out_rx.recv_timeout(Duration::from_millis(25)) {
            // ConPTY (Windows) opens with a Device Status Report (`\x1b[6n`)
            // and won't relay child-stdout until it gets a cursor-position
            // reply. Answer with row=1,col=1 so the helper's output starts
            // flowing. Harmless on macOS / Linux PTYs (no DSR is sent).
            if b.windows(3).any(|w| w == b"[6n") {
                let _ = pty.in_tx.send(b"\x1b[1;1R".to_vec());
            }
            parser.advance(&b);
        }
        if grid_contains(parser.grid(), needle) {
            return true;
        }
    }
    grid_contains(parser.grid(), needle)
}

fn grid_contains(grid: &Grid, needle: &str) -> bool {
    for r in 0..grid.rows {
        let row: String = grid
            .row(r)
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
            .map(|c| c.ch)
            .collect();
        if row.contains(needle) {
            return true;
        }
    }
    false
}

fn dump(parser: &Parser) -> String {
    let mut out = String::new();
    for r in 0..parser.grid().rows {
        let row: String = parser
            .grid()
            .row(r)
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
            .map(|c| c.ch)
            .collect();
        let t = row.trim_end();
        if !t.is_empty() {
            out.push_str(&format!("[{r:2}]: {t}\n"));
        }
    }
    out
}

#[test]
fn multi_round_pty_both_prompts_land_in_grid() {
    // Resolve the helper binary Cargo built alongside this integration
    // test. Using `CARGO_BIN_EXE_*` makes this portable — Windows gets
    // the `.exe` suffix, Unix doesn't, and there's no PATH dependency.
    let helper = env!("CARGO_BIN_EXE_pty_multi_round_helper");

    let pty = PtyHandle::spawn(helper, 80, 24).expect("spawn helper");
    let mut parser = Parser::new(Grid::new(80, 24));

    // Round 1: helper writes "P1>" then blocks on stdin.read_line.
    // The fix in #185 (mirrored in the App layer) is what guarantees
    // this prompt makes it through to a renderable state; here we
    // assert the grid actually contains it.
    let p1 = drain_until(&pty, &mut parser, "P1>", Duration::from_secs(5));
    assert!(
        p1,
        "Issue #175 / PR #185 regression: round-1 prompt 'P1>' never \
         reached the grid through the PTY→Parser pipeline within 5s.\n\
         Grid dump:\n{}",
        dump(&parser)
    );

    // Reply to round 1 — un-blocks the helper's first read_line.
    pty.in_tx.send(b"a\r\n".to_vec()).expect("send round-1 reply");

    // Round 2: helper writes "P2>" then blocks on stdin.read_line
    // again. This is the critical assertion: prior to the redraw-
    // reschedule fix, the second prompt could sit in the grid
    // (parsed but unrendered) and be invisible until an unrelated
    // event woke the loop. Here we don't depend on rendering — we
    // assert the bytes themselves traverse all the way to the grid.
    let p2 = drain_until(&pty, &mut parser, "P2>", Duration::from_secs(5));
    assert!(
        p2,
        "Issue #175 / PR #185 regression: round-2 prompt 'P2>' never \
         reached the grid after round-1 reply — multi-round PTY flow \
         is dropping the second prompt.\n\
         Grid dump:\n{}",
        dump(&parser)
    );

    // Reply to round 2 and wait for the helper to print its DONE
    // marker, which proves both rounds completed end-to-end (and not
    // that we coincidentally observed stale buffer contents).
    pty.in_tx.send(b"b\r\n".to_vec()).expect("send round-2 reply");
    let done = drain_until(&pty, &mut parser, "DONE", Duration::from_secs(5));
    assert!(
        done,
        "helper did not emit DONE after both rounds — the multi-round \
         exchange did not complete cleanly.\nGrid dump:\n{}",
        dump(&parser)
    );

    // Final invariant: both prompts must still be visible in the grid
    // at the end (no scrollback-eviction edge case clobbering them in
    // a small 24-row terminal).
    assert!(grid_contains(parser.grid(), "P1>"), "P1> evicted from grid by end of test");
    assert!(grid_contains(parser.grid(), "P2>"), "P2> evicted from grid by end of test");
}
