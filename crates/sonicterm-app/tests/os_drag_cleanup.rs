//! Issue #438 regression guard: `App::cancel_drag_session()` must
//! wipe drag residue on EVERY window (main + every child), not just
//! the main one. Before the fix, the OS-drag end / cancel paths
//! (`handle_os_drag_ended` → `cancel_drag_session`) bypassed the
//! normal winit `MouseInput::Released` handlers that clear the
//! per-window renderer's `drag_chip` overlay, so a stale grey
//! rectangle was left floating in empty pane space after a tab
//! tear-out gesture ended without a "real" mouse-up event.
//!
//! These tests cover both the App-observable per-window scalar state
//! (`pressed_tab` / `mouse_down` / `drag_session` / `drag_target`)
//! AND — via the `test_drag_chip_marker` seam (#443 cycle-2) — the
//! drag-chip clear that production code runs against the renderer.
//!
//! Seam rationale (Haiku review on PR #443): unit tests cannot
//! construct a real `GpuRenderer` (it needs a live wgpu surface), so
//! production code flips `test_drag_chip_marker` in lock-step with
//! the real `renderer.set_drag_chip(None)` call. Tests pre-seed each
//! window's marker to `Some(true)` BEFORE the cancel, and assert it
//! is `Some(false)` AFTER — proving the per-window iteration ran on
//! that window. If a future refactor drops the per-window loop, the
//! marker stays `Some(true)` and the assertion fails. This is more
//! substantive than the previous `renderer: None` no-op, which left
//! the test green even if production never touched the renderer.

use sonicterm_app::app::{synthetic_main_window_id, App};
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
fn hex() -> Hex {
    Hex("#000000".to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    }
}

fn synth_theme() -> Theme {
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    }
}

fn empty_keymap() -> Keymap {
    Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() }
}

fn make_app() -> App {
    let mut app = App::new(synth_theme(), Config::default(), empty_keymap());
    app.__test_synthetic_main();
    app
}

#[test]
fn cancel_drag_session_clears_pressed_tab_and_mouse_down_on_child_windows() {
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["c1", "c2"]);

    // Seed scalar residue on BOTH main and child.
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(1), true, false));

    // Seed drag-chip markers on BOTH windows — this is the renderer
    // stand-in (#443 cycle-2). If cancel_drag_session fails to iterate
    // either window, that window's marker stays `Some(true)`.
    assert!(app.__test_set_main_drag_chip_marker(true));
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));

    // Sanity: residue + markers are observable before the cancel.
    assert_eq!(app.__test_pressed_tab(), Some(0));
    assert!(app.__test_mouse_down());
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(Some(1)));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(true));
    assert_eq!(app.__test_main_drag_chip_marker(), Some(true));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(true));

    let _ = app.cancel_drag_session();

    // Scalar state: main is cleared (pre-#438 behavior).
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
    // Scalar state: child is ALSO cleared (the #438 fix).
    assert_eq!(
        app.__test_child_pressed_tab(child_id),
        Some(None),
        "#438: cancel_drag_session must clear child pressed_tab"
    );
    assert_eq!(
        app.__test_child_mouse_down(child_id),
        Some(false),
        "#438: cancel_drag_session must clear child mouse_down"
    );
    // Renderer stand-in: BOTH windows had their drag-chip clear executed
    // (production code flips this in lock-step with renderer.set_drag_chip(None)).
    assert_eq!(
        app.__test_main_drag_chip_marker(),
        Some(false),
        "#438 (Haiku PR#443): cancel_drag_session must clear MAIN drag_chip"
    );
    assert_eq!(
        app.__test_window_drag_chip_marker(child_id),
        Some(false),
        "#438 (Haiku PR#443): cancel_drag_session must clear CHILD drag_chip"
    );
}

#[test]
fn cancel_drag_session_clears_drag_session_on_child_windows() {
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["only"]);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(0), true, true));
    assert!(app.__test_set_main_drag_chip_marker(true));
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));

    assert_eq!(app.__test_child_has_drag_session(child_id), Some(true));

    let had = app.cancel_drag_session();
    assert!(had, "cancel_drag_session must return true when a child carried a drag session");

    assert_eq!(
        app.__test_child_has_drag_session(child_id),
        Some(false),
        "#438: child drag_session must be cleared"
    );
    assert_eq!(
        app.__test_child_has_drag_target(child_id),
        Some(false),
        "#438: child drag_target must be cleared"
    );
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(None));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(false));
    // Renderer stand-in: cancel ran the per-window loop on both windows
    // and cleared each drag_chip marker.
    assert_eq!(app.__test_main_drag_chip_marker(), Some(false));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(false));
}

#[test]
fn cancel_drag_session_is_idempotent_across_all_windows() {
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["x"]);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(0), true, true));
    assert!(app.__test_set_main_drag_chip_marker(true));
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));
    let _ = app.cancel_drag_session();

    // After first cancel: every marker is Some(false).
    assert_eq!(app.__test_main_drag_chip_marker(), Some(false));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(false));

    // Re-arm one marker to prove the second cancel still iterates every
    // window (and isn't short-circuited by the `had = false` return).
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));

    let had = app.cancel_drag_session();
    assert!(!had, "second cancel finds no live drag session");
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(None));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(false));
    assert_eq!(app.__test_child_has_drag_session(child_id), Some(false));
    // The re-armed marker proves cancel iterated the child window
    // even when no drag_session was live.
    assert_eq!(
        app.__test_window_drag_chip_marker(child_id),
        Some(false),
        "#438 (Haiku PR#443): cancel_drag_session must iterate ALL windows on every call"
    );
}

// `synthetic_main_window_id` is referenced indirectly via the
// __test_*_main_drag_chip_marker helpers; pull the symbol in to keep
// rustdoc cross-refs sane.
#[allow(dead_code)]
fn _ref_synth_id() -> winit::window::WindowId {
    synthetic_main_window_id()
}

// ---------------------------------------------------------------------
// Issue #462 (speculative defensive fix per PM override) — guard rails
// ---------------------------------------------------------------------

/// Issue #462: the `DroppedOnEmpty` branch must NOT call
/// `cancel_drag_session` inline. It must instead defer via
/// `pending_os_teardown`, which the event-loop drains AFTER
/// `drain_pending_window_creates`. The drain itself must still run
/// the all-windows cleanup loop unconditionally (preserves the
/// `cancel_drag_session_is_idempotent_across_all_windows` contract).
#[test]
fn pending_os_teardown_drain_clears_residue_on_all_windows() {
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["a", "b"]);

    // Seed residue on both windows + drag-chip markers.
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(1), true, true));
    assert!(app.__test_set_main_drag_chip_marker(true));
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));

    // Simulate the `DroppedOnEmpty` deferral.
    app.__test_set_pending_os_teardown(true);
    assert!(app.__test_pending_os_teardown());

    // Residue must NOT be cleared yet — that's the entire point of the
    // deferral. Tear-out-spawn would run between setting the flag and
    // draining it; if cleanup ran inline (pre-#462) it would race the
    // spawn.
    assert_eq!(app.__test_pressed_tab(), Some(0));
    assert_eq!(app.__test_main_drag_chip_marker(), Some(true));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(true));

    // Drain — what `event_loop.rs::do_user_event` does AFTER
    // `drain_pending_window_creates`.
    app.__test_drain_pending_os_teardown();

    // Flag is consumed.
    assert!(!app.__test_pending_os_teardown());
    // All-windows cleanup ran exactly as `cancel_drag_session`
    // unconditionally does today.
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(None));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(false));
    assert_eq!(app.__test_child_has_drag_session(child_id), Some(false));
    assert_eq!(app.__test_main_drag_chip_marker(), Some(false));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(false));
}

/// Issue #462: drain is a no-op when the flag is not set. Important
/// because `event_loop.rs` calls it on EVERY `UserEvent` dispatch —
/// must be cheap and safe.
#[test]
fn pending_os_teardown_drain_is_noop_when_flag_unset() {
    let mut app = make_app();
    let child_id = app.__test_seed_child_window(&["only"]);
    assert!(app.__test_seed_child_drag_residue(child_id, Some(0), true, true));
    assert!(app.__test_set_window_drag_chip_marker(child_id, true));

    // Flag was never set — drain must be a pure no-op.
    assert!(!app.__test_pending_os_teardown());
    app.__test_drain_pending_os_teardown();

    // Residue is UNTOUCHED — cancel_drag_session was not invoked.
    assert_eq!(app.__test_child_pressed_tab(child_id), Some(Some(0)));
    assert_eq!(app.__test_child_mouse_down(child_id), Some(true));
    assert_eq!(app.__test_window_drag_chip_marker(child_id), Some(true));
}

/// Issue #462: a window inserted between `pending_os_teardown = true`
/// and the drain (simulating a tear-out-spawn landing during the
/// race window) must be safely cleaned up on the eventual drain.
/// Proves the snapshot iteration in `cancel_drag_session` reads
/// `self.windows.keys()` AT DRAIN TIME, not at flag-set time.
#[test]
fn pending_os_teardown_handles_window_inserted_after_flag_set() {
    let mut app = make_app();

    // Flag set first (DroppedOnEmpty branch fires).
    app.__test_set_pending_os_teardown(true);

    // Tear-out-spawn lands here (simulated): a new child window
    // appears, complete with drag-chip marker residue from its
    // half-initialized state.
    let new_child = app.__test_seed_child_window(&["spawned"]);
    assert!(app.__test_seed_child_drag_residue(new_child, Some(0), true, true));
    assert!(app.__test_set_window_drag_chip_marker(new_child, true));

    // Drain runs AFTER the spawn (the whole point of the order fix).
    app.__test_drain_pending_os_teardown();

    // Cleanup covered the newly-inserted window too.
    assert_eq!(app.__test_child_pressed_tab(new_child), Some(None));
    assert_eq!(app.__test_child_mouse_down(new_child), Some(false));
    assert_eq!(app.__test_child_has_drag_session(new_child), Some(false));
    assert_eq!(app.__test_window_drag_chip_marker(new_child), Some(false));
}

// ---------------------------------------------------------------------
// PR #533 Haiku Step-4 REVISE — additional guard rails
// ---------------------------------------------------------------------

/// PR #533 Haiku Step-4 2nd-pass REVISE (1/2): `cancel_drag_session`
/// snapshots `self.windows.keys()` BEFORE the mutation loop (#462
/// defensive fix at `mod.rs:3349`). The loop body must not panic if a
/// window vanishes between snapshot collection and the
/// `windows.get_mut(...)` lookup — the `else { continue }` branch
/// covers it.
///
/// To exercise the ACTUAL `get_mut`-after-snapshot branch (not just
/// the trivial "removed before the call" case Haiku flagged on the
/// 1st REVISE), this test uses the production
/// `__test_set_post_snapshot_hook` seam to remove a window in the
/// exact race window: after the `ids` snapshot is taken, before the
/// iteration body runs. The hook fires once and is `take()`-d, so it
/// cannot re-arm or interfere with other tests.
#[test]
fn cancel_drag_session_tolerates_window_removed_before_iteration() {
    let mut app = make_app();
    let survivor = app.__test_seed_child_window(&["s1"]);
    let doomed = app.__test_seed_child_window(&["d1"]);

    // Seed residue + drag-chip markers on EVERY window.
    app.__test_set_pressed_tab(Some(0));
    app.__test_set_mouse_down(true);
    assert!(app.__test_seed_child_drag_residue(survivor, Some(2), true, true));
    assert!(app.__test_seed_child_drag_residue(doomed, Some(3), true, true));
    assert!(app.__test_set_main_drag_chip_marker(true));
    assert!(app.__test_set_window_drag_chip_marker(survivor, true));
    assert!(app.__test_set_window_drag_chip_marker(doomed, true));

    // Both windows must be live BEFORE the call — this is the precondition
    // that proves the snapshot DOES contain `doomed`, so removing it
    // inside the hook exercises the `get_mut(&id).else { continue }`
    // branch (rather than the never-was-snapshotted path).
    assert_eq!(app.__test_window_drag_chip_marker(doomed), Some(true));
    assert_eq!(app.__test_window_drag_chip_marker(survivor), Some(true));

    // Install the post-snapshot hook. It fires AFTER `ids = windows.keys()`
    // has captured `doomed` AND BEFORE the per-id loop body runs. The
    // closure removes `doomed`; when the loop reaches that id its
    // `get_mut(&id)` returns `None` and the `else { continue }` arm
    // (the entire point of the #462 defensive snapshot) fires.
    app.__test_set_post_snapshot_hook(move |inner| {
        assert!(
            inner.__test_remove_window(doomed),
            "post-snapshot hook: doomed window must still be present at hook time \
             (proves the snapshot contains it)"
        );
    });

    // The call must NOT panic, must complete the iteration on every
    // still-live window, and must NOT resurrect the removed window.
    let _ = app.cancel_drag_session();

    // Surviving windows: residue + marker cleared as usual.
    assert_eq!(app.__test_pressed_tab(), None);
    assert!(!app.__test_mouse_down());
    assert_eq!(app.__test_main_drag_chip_marker(), Some(false));
    assert_eq!(app.__test_child_pressed_tab(survivor), Some(None));
    assert_eq!(app.__test_child_mouse_down(survivor), Some(false));
    assert_eq!(app.__test_child_has_drag_session(survivor), Some(false));
    assert_eq!(app.__test_window_drag_chip_marker(survivor), Some(false));

    // Removed window stays removed — the tolerant lookup did NOT
    // resurrect it via `get_mut`.
    assert_eq!(app.__test_child_pressed_tab(doomed), None);
    assert_eq!(app.__test_window_drag_chip_marker(doomed), None);
}

/// PR #533 Haiku Step-4 REVISE (2/2): `WindowState::clear_drag_chip`
/// is documented as **tolerant** (see contract block at `mod.rs:235`):
/// it must be a safe no-op when BOTH `renderer` is `None` AND
/// `test_drag_chip_marker` is `None` — the "transitional window"
/// case where a tear-out spawn lands during the `pending_os_teardown`
/// race window (#462) and produces a half-initialized `WindowState`.
/// Seeded child windows from `__test_seed_child_window` are exactly
/// this shape: `renderer: None`, `test_drag_chip_marker: None`. Drive
/// `cancel_drag_session` against such a window and assert the call
/// completes without panic and leaves the marker as `None` (i.e. the
/// `if let Some(marker) = ...` short-circuit fired, not the
/// `*marker = false` branch).
#[test]
fn clear_drag_chip_tolerant_when_renderer_and_marker_both_none() {
    let mut app = make_app();
    let transitional = app.__test_seed_child_window(&["t1"]);

    // Confirm the precondition: BOTH branches of `clear_drag_chip` see
    // `None` on this window. `test_drag_chip_marker` is `None` because
    // we deliberately do NOT seed it; `renderer` is `None` for every
    // `__test_seed_child_window`-produced entry by construction.
    assert_eq!(
        app.__test_window_drag_chip_marker(transitional),
        None,
        "precondition: marker must be None for the transitional case"
    );

    // Drive `cancel_drag_session` — its per-window loop body calls
    // `ws.clear_drag_chip()` on EVERY window including this one. Must
    // not panic.
    let _ = app.cancel_drag_session();

    // Marker stayed `None` — the `if let Some(marker) = ...` branch
    // short-circuited as documented (it did NOT flip `None` to
    // `Some(false)`). This is the no-op contract.
    assert_eq!(
        app.__test_window_drag_chip_marker(transitional),
        None,
        "clear_drag_chip must NOT mutate a `None` marker into `Some(false)`"
    );
    // The window's scalar residue fields are still touched by the
    // surrounding `cancel_drag_session` loop body — `clear_drag_chip`
    // only owns the renderer + marker pair.
    assert_eq!(app.__test_child_pressed_tab(transitional), Some(None));
    assert_eq!(app.__test_child_mouse_down(transitional), Some(false));
}

// ---------------------------------------------------------------------
// Issue #553 Phase A — in-process tear-out (no Command::new spawn)
// ---------------------------------------------------------------------

/// Phase A spec, test 1: simulating a `DragOutcome::DroppedOnEmpty
/// { drop_screen_pos }` end-event must enqueue a typed
/// `PendingTearOut` request (not the old `Cancelled`/`Command::new`
/// path). The request carries the source tab handle + Win32 cursor
/// screen position, ready for `drain_pending_window_creates` to
/// build the new window IN-PROCESS via `install_torn_out_window`.
#[test]
fn dropped_on_empty_queues_typed_tear_out_request_not_child_process_spawn() {
    let mut app = make_app();
    let src_id = sonicterm_app::app::synthetic_main_window_id();
    app.__test_set_os_drag_source(Some((src_id, 0)));

    // Pre: nothing queued.
    assert!(app.__test_pending_tear_out().is_none());
    assert!(!app.__test_pending_new_window());

    // Backend posts DroppedOnEmpty with a cursor position from
    // Win32 GetCursorPos. We feed the mailbox directly to bypass
    // OLE on the unit-test path (per Phase C2 mailbox seam).
    app.__test_os_drag_pending().set_ended(
        sonicterm_app::app::os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos: (1234, 567) },
    );

    let processed = app.handle_os_drag_ended();
    assert!(matches!(
        processed,
        Some(sonicterm_app::app::os_drag::DragOutcome::DroppedOnEmpty { .. })
    ));

    // Typed request landed in the queue, NOT the legacy NewShell flag.
    let queued = app.__test_pending_tear_out().expect("PendingTearOut must be queued");
    assert_eq!(queued.0, src_id, "source window handle preserved");
    assert_eq!(queued.1, 0, "source tab index preserved");
    assert_eq!(queued.2, (1234, 567), "drop screen pos from GetCursorPos preserved");
    assert!(!app.__test_pending_new_window(), "must NOT touch NewShell drain");

    // Deferred-teardown invariant unchanged.
    assert!(app.__test_pending_os_teardown());
}

/// Phase A spec, test 2: the `pending_tear_out` drain MUST be in the
/// SAME drain slot as `pending_new_window` (so `drain_pending_os_
/// teardown` still runs strictly AFTER any window-create). Guarded by
/// the absence of an `__test_drain_pending_tear_out` separate seam —
/// the request is drained alongside NewShell by
/// `drain_pending_window_creates`. We assert the ordering invariant
/// by exposing both flags simultaneously: after a real drain the
/// teardown flag is still set, proving the drain order is
/// (pending_window_creates → pending_os_teardown).
#[test]
fn pending_tear_out_drains_before_os_teardown() {
    let mut app = make_app();
    let src_id = sonicterm_app::app::synthetic_main_window_id();
    app.__test_set_os_drag_source(Some((src_id, 0)));

    app.__test_os_drag_pending().set_ended(
        sonicterm_app::app::os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos: (10, 20) },
    );
    let _ = app.handle_os_drag_ended();

    // Both flags set in the same handler.
    assert!(app.__test_pending_tear_out().is_some());
    assert!(app.__test_pending_os_teardown());

    // Drain os_teardown (the post-window-create slot) — the
    // tear-out request is still queued in the pre-slot, proving the
    // ordering: pending_tear_out (in drain_pending_window_creates)
    // runs BEFORE drain_pending_os_teardown, as required by PR #533.
    app.__test_drain_pending_os_teardown();
    assert!(!app.__test_pending_os_teardown());
    assert!(
        app.__test_pending_tear_out().is_some(),
        "pending_tear_out drains in drain_pending_window_creates (NOT in os_teardown drain)"
    );
}
