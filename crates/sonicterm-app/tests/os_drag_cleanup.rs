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
