//! PR #133 regression: a PTY-burst redraw within the vsync window must
//! bypass the coalescing gate. PR #132 added an `input_dirty` bypass
//! for keyboard/mouse/theme events but left streaming-output redraws
//! gated, which capped sonic at 1 frame per refresh interval (~14 ms
//! on 60 Hz) on terminal-throughput workloads. vtebench scrolling
//! showed sonic 6–66× slower than wezterm as a direct result. The fix
//! introduces a `pty_burst_gen: Arc<AtomicU32>` counter the VT thread
//! increments on every non-empty byte chunk; the renderer snapshots it
//! on each `RedrawRequested` and records only that snapshot after the
//! render returns.
//!
//! These tests pin the contract by exercising `would_coalesce_redraw`
//! (the exact predicate used by the `RedrawRequested` arm) with the
//! same simulated `last_render`, `pty_burst_gen`, and
//! `last_seen_burst_gen` states the real event loop would have.

use std::time::{Duration, Instant};

use sonicterm_app::app::App;
use sonicterm_cfg::{
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
fn pty_burst_gen_starts_at_zero() {
    let a = app();
    assert_eq!(a.pty_burst_gen_for_test(), 0, "fresh App must not have a stale PTY burst");
    assert_eq!(
        a.last_seen_burst_gen_for_test(),
        0,
        "fresh App must start with no seen PTY burst generation"
    );
}

#[test]
fn pty_burst_bypasses_the_gate_even_immediately_after_a_render() {
    let mut a = app();
    // Simulate "we rendered ~100us ago" — well inside the ~16.667 ms
    // frame_period — with no user input but a fresh PTY burst.
    a.set_last_render_for_test(Instant::now() - Duration::from_micros(100));
    a.mark_pty_burst_for_test();
    assert!(
        !a.would_coalesce_redraw(),
        "a PTY-burst redraw within the vsync window MUST render immediately, \
         not coalesce — this was the cause of the 6–66× wezterm regression"
    );
}

#[test]
fn no_pty_burst_and_no_input_within_frame_period_still_coalesces() {
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_millis(1));
    // Neither flag set — pure timer-driven redraw (e.g. cursor blink
    // wakeup) inside the vsync window should still coalesce so idle
    // CPU stays near 0%.
    assert!(
        a.would_coalesce_redraw(),
        "with no PTY burst and no input, a redraw inside the vsync window must coalesce"
    );
}

#[test]
fn seen_pty_burst_generation_coalesces_until_another_burst_arrives() {
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_millis(1));
    a.mark_pty_burst_for_test();
    let snapshot = a.pty_burst_gen_for_test();
    a.mark_burst_gen_seen_for_test(snapshot);

    assert!(
        a.would_coalesce_redraw(),
        "once render has seen a PTY burst generation, timer redraws inside the vsync window coalesce"
    );

    a.mark_pty_burst_for_test();
    assert!(!a.would_coalesce_redraw(), "a later PTY burst generation must bypass the vsync gate");
}

#[test]
fn input_dirty_still_wins_independently_of_pty_burst() {
    // Belt-and-braces: even if the pty_burst path regressed, the
    // PR #132 input bypass must remain intact.
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_micros(100));
    a.mark_input_dirty_for_test();
    assert!(
        !a.would_coalesce_redraw(),
        "input_dirty bypass from PR #132 must continue to render immediately"
    );
}

#[test]
fn vt_burst_during_render_is_not_erased_by_seen_snapshot() {
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_millis(1));

    // Render snapshots generation 1 at the start of RedrawRequested.
    a.mark_pty_burst_for_test();
    let render_snapshot = a.pty_burst_gen_for_test();

    // The VT thread receives more bytes while that render is in flight.
    a.mark_pty_burst_for_test();

    // Render completion must mark only the starting snapshot as seen.
    // The current generation is now ahead, so the next redraw must render
    // immediately instead of waiting for the next vsync gate.
    a.mark_burst_gen_seen_for_test(render_snapshot);
    assert!(
        !a.would_coalesce_redraw(),
        "a PTY burst that arrives during render must remain pending for the next redraw"
    );
}
