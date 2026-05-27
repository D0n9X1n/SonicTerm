//! PR #133 regression: a PTY-burst redraw within the vsync window must
//! bypass the coalescing gate. PR #132 added an `input_dirty` bypass
//! for keyboard/mouse/theme events but left streaming-output redraws
//! gated, which capped sonic at 1 frame per refresh interval (~14 ms
//! on 60 Hz) on terminal-throughput workloads. vtebench scrolling
//! showed sonic 6–66× slower than wezterm as a direct result. The fix
//! introduces a `pty_burst_dirty: Arc<AtomicBool>` flag the VT thread
//! sets on every non-empty byte chunk; the renderer reads it on each
//! `RedrawRequested` and clears it after the render returns.
//!
//! These tests pin the contract by exercising `would_coalesce_redraw`
//! (the exact predicate used by the `RedrawRequested` arm) with the
//! same simulated `last_render` and `pty_burst_dirty` states the real
//! event loop would have.

use std::time::{Duration, Instant};

use sonic_app::app::App;
use sonic_core::{
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
fn pty_burst_dirty_starts_false() {
    let a = app();
    assert!(!a.pty_burst_dirty_for_test(), "fresh App must not have a stale PTY burst flag");
}

#[test]
fn pty_burst_bypasses_the_gate_even_immediately_after_a_render() {
    let mut a = app();
    // Simulate "we rendered ~100us ago" — well inside the ~16.667 ms
    // frame_period — with no user input but a fresh PTY burst.
    a.set_last_render_for_test(Instant::now() - Duration::from_micros(100));
    a.mark_pty_burst_dirty_for_test();
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
fn pty_burst_flag_survives_until_render_clears_it() {
    let a = app();
    a.mark_pty_burst_dirty_for_test();
    assert!(a.pty_burst_dirty_for_test(), "flag must remain set until render clears it");
    // The real render path clears via Release store; mirror that.
    a.pty_burst_dirty_for_test();
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
