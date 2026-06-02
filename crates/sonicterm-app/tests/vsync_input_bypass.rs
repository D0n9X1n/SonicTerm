//! PR #132 regression: input-driven redraws must bypass the vsync
//! coalescing gate so the first frame after typing/resize/theme is
//! immediate (zero added latency). Only purely pty-byte-driven
//! redraws within the current `frame_period` get coalesced onto the
//! next vsync boundary.

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
fn pty_only_redraw_within_frame_period_is_coalesced() {
    let mut a = app();
    // Simulate "we just rendered ~1ms ago" with no user input since.
    a.set_last_render_for_test(Instant::now() - Duration::from_millis(1));
    assert!(
        a.would_coalesce_redraw(),
        "a pty-byte redraw inside the vsync window must coalesce to next vsync"
    );
}

#[test]
fn input_dirty_bypasses_the_gate_even_immediately_after_a_render() {
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_micros(100));
    a.mark_input_dirty_for_test();
    assert!(
        !a.would_coalesce_redraw(),
        "an input event must render immediately (zero added latency), not coalesce"
    );
}

#[test]
fn after_full_frame_period_pty_redraw_renders_immediately() {
    let mut a = app();
    a.set_last_render_for_test(Instant::now() - Duration::from_millis(50));
    assert!(
        !a.would_coalesce_redraw(),
        "once frame_period has elapsed, even a pty-driven redraw renders now"
    );
}
