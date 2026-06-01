//! Issue #175 regression: PTY hangs / output disappears on commands with
//! multiple output→input rounds (e.g. `gh auth login`'s device-code flow).
//!
//! Root cause: the `RedrawRequested` arm in `window_event.rs` (and the
//! mirrored arm in `child_window.rs`) used `pane.parser.try_lock()` to
//! avoid AB-BA deadlocking with the VT thread, but the failure arm just
//! `return`ed without rescheduling. If the VT thread held the lock when
//! the user-triggered redraw arrived (very likely during a fast
//! input→output transition: keystroke wakes the loop, VT thread is
//! mid-parse on the response), the entire redraw was dropped silently.
//! The parsed bytes sat in the grid until some *unrelated* event (Ctrl+C
//! generating output, a mouse move setting input_dirty) drove a fresh
//! `RedrawRequested` — matching the user-visible symptom that "Ctrl+C
//! unsticks it and a flurry of missed output appears."
//!
//! The fix marks `pending_redraw = true` (and preserves the captured
//! `input_dirty` snapshot) so `about_to_wait` schedules a `WaitUntil`
//! at the next vsync deadline and `do_new_events`' `ResumeTimeReached`
//! arm re-requests the redraw. The deferred render attempt then
//! succeeds because the VT thread releases the parser lock within
//! microseconds (it never holds across a winit call).

use std::time::{Duration, Instant};

use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

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

fn app() -> App {
    App::new(synth_theme(), Config::default(), empty_keymap())
}

#[test]
fn lock_contention_bail_marks_pending_redraw_with_input_dirty_preserved() {
    // Simulate the bail-out arm of the `RedrawRequested` handler when
    // `pane.parser.try_lock()` returns `None`. Before the fix, this
    // path just `return`ed and `pending_redraw` stayed `false`, so
    // `about_to_wait` saw nothing to schedule and the redraw request
    // was lost. After the fix, `pending_redraw` must be set so the
    // next vsync tick re-attempts the render.
    let mut a = app();
    assert!(!a.pending_redraw_for_test(), "fresh App must start with no pending redraw");

    // Reproduce the captured-`was_dirty` snapshot the real handler
    // takes at the top: user typed a key, so input_dirty was true.
    let was_dirty = true;
    a.defer_redraw_on_lock_contention(was_dirty);

    assert!(
        a.pending_redraw_for_test(),
        "Issue #175: lock-contention bail-out MUST mark pending_redraw \
         so about_to_wait schedules a follow-up vsync wake — otherwise \
         the redraw request is dropped silently and the next prompt \
         frame never paints until an unrelated event (Ctrl+C, mouse \
         move) triggers a fresh RedrawRequested"
    );
    assert!(
        a.input_dirty_for_test(),
        "input_dirty captured at the top of RedrawRequested must be \
         preserved across the bail-out so the rescheduled redraw still \
         bypasses the vsync coalescing gate (otherwise the deferred \
         redraw would coalesce to the next 16ms boundary and add \
         user-visible latency on the input→output transition)"
    );
}

#[test]
fn lock_contention_bail_preserves_was_dirty_false_for_pure_pty_redraws() {
    // The bail-out path must NOT spuriously set input_dirty: a redraw
    // triggered purely by streaming PTY bytes (was_dirty == false at
    // the top of the handler) must remain a coalescing-eligible redraw
    // when rescheduled, so idle-output bursts don't burn an extra
    // immediate render on top of the next vsync render.
    let mut a = app();
    a.set_pending_redraw_for_test(false);
    a.defer_redraw_on_lock_contention(false);
    assert!(a.pending_redraw_for_test(), "still must reschedule");
    assert!(
        !a.input_dirty_for_test(),
        "was_dirty=false must be preserved — a pure-PTY redraw that \
         bailed on lock contention must NOT be promoted to an \
         input-dirty redraw, that would defeat the vsync coalescing \
         gate for streaming output"
    );
}

#[test]
fn lock_contention_bail_is_idempotent_if_already_pending() {
    // Multiple back-to-back bail-outs (e.g. several rapid PTY bursts
    // all racing the same in-flight parse) must converge to a single
    // pending_redraw — no double-fire, no flag flip-flop.
    let mut a = app();
    a.set_pending_redraw_for_test(true);
    a.defer_redraw_on_lock_contention(true);
    a.defer_redraw_on_lock_contention(true);
    a.defer_redraw_on_lock_contention(false);
    assert!(a.pending_redraw_for_test());
    assert!(
        !a.input_dirty_for_test(),
        "the most recent was_dirty snapshot wins — the last bail-out \
         was a pure-PTY redraw, so it must clear an earlier dirty flag \
         set by a stale snapshot from a previous bail-out"
    );
}

#[test]
fn about_to_wait_path_will_fire_after_lock_contention_bail() {
    // Belt-and-braces: pair the bail-out with the predicate
    // `about_to_wait` uses to decide whether to schedule a WaitUntil.
    // The contract is "pending_redraw == true ⇒ schedule wake at
    // last_render + frame_period". A bail-out that doesn't set
    // pending_redraw silently breaks the wake chain.
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_micros(500));
    a.defer_redraw_on_lock_contention(false);
    assert!(
        a.pending_redraw_for_test(),
        "the bail-out + vsync-wake handshake REQUIRES pending_redraw \
         to be set — about_to_wait only schedules WaitUntil when this \
         flag is true (see event_loop.rs::do_about_to_wait)"
    );
}
