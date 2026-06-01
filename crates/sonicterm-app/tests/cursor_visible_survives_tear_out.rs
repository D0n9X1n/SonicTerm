//! PR #400 regression — `cursor_visible` must travel with the pane on
//! tear-out.
//!
//! Pre-fix: `cursor_visible` lived on `WindowState`. When a tab was
//! torn out into a new window, the destination `WindowState` was built
//! with a fresh `Arc<AtomicBool>` while the moved pane's VT thread
//! kept writing to the source window's old Arc. Reads on the new
//! window's render thread therefore never observed the DECTCEM flag
//! the shell emitted on the moved pane.
//!
//! Post-fix: `cursor_visible` lives on `PaneState`. The Arc moves with
//! the pane (the whole `PaneState` is `HashMap::remove`d from source
//! and `insert`ed into destination on tear-out), so writes from the
//! pane's VT thread continue to land on the SAME Arc the destination
//! window's render path reads from.
//!
//! This test asserts the structural invariant: each `PaneState` owns
//! its own `cursor_visible` Arc, and a clone held "as if by the VT
//! thread" still observes stores after the pane has been
//! removed/inserted across `HashMap`s (the operation tear-out
//! performs).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_app::app::PaneState;
use sonicterm_core::{grid::Grid, vt::Parser};

fn make_pane() -> PaneState {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    PaneState::new(parser, None)
}

#[test]
fn pane_cursor_visible_arc_survives_hashmap_move() {
    // Source window's pane map.
    let mut source_panes: HashMap<u64, PaneState> = HashMap::new();
    let pane_id: u64 = 7;
    let pane = make_pane();
    // Simulate the VT thread capturing the Arc clone before the pane
    // is owned by any window's map.
    let vt_thread_clone = pane.cursor_visible.clone();
    source_panes.insert(pane_id, pane);

    // Tear-out: pane moves from source to destination window.
    let moved = source_panes.remove(&pane_id).expect("pane present in source");
    let mut dest_panes: HashMap<u64, PaneState> = HashMap::new();
    dest_panes.insert(pane_id, moved);

    // VT thread (still alive, still holding its Arc clone) emits a
    // DECTCEM-off after the move. The destination window's render
    // path reads `dest.panes[&pane_id].cursor_visible` — these MUST
    // be the same allocation.
    vt_thread_clone.store(false, Ordering::Relaxed);

    let pane_now = dest_panes.get(&pane_id).expect("pane present in destination");
    assert!(
        Arc::ptr_eq(&pane_now.cursor_visible, &vt_thread_clone),
        "destination pane's cursor_visible must be the SAME Arc the VT thread writes to",
    );
    assert!(
        !pane_now.cursor_visible.load(Ordering::Relaxed),
        "render path on destination window must observe the VT thread's DECTCEM-off store",
    );

    // And of course: a fresh, unrelated pane's Arc is NOT affected
    // (per-pane independence, not per-window).
    let other = make_pane();
    assert!(
        other.cursor_visible.load(Ordering::Relaxed),
        "unrelated pane's cursor_visible must default to true",
    );
    assert!(
        !Arc::ptr_eq(&other.cursor_visible, &pane_now.cursor_visible),
        "each pane owns its own Arc — no accidental sharing",
    );
}

#[test]
fn pane_cursor_visible_default_is_visible() {
    let pane = make_pane();
    assert!(
        pane.cursor_visible.load(Ordering::Relaxed),
        "PaneState::new must initialize cursor_visible to true",
    );
}
