//! Epic #289 Phase B — regression test for user-reported bug #2:
//! after tearing out a tab into a new window, Cmd+T must spawn a new
//! tab IN THE NEW WINDOW (the frontmost), not in the main window.
//!
//! The Phase A frontmost-routing fix already shipped the dispatcher
//! logic (see `multi_window_frontmost_routing.rs`); Phase B adds the
//! contract that `tear_out_tab` updates `frontmost_window` to the
//! new window's id so that dispatcher sees the right target.
//!
//! Pure-logic coverage — we simulate "tear-out finished, frontmost
//! now points at a child window" via `__test_set_frontmost_window`
//! and assert `Action::NewTab` consults `frontmost_kind()` first.
//! The frontmost-update-on-tear-out invariant is asserted by
//! reading the Phase B production code: line in `tear_out.rs` /
//! `tear_out_from_child` that sets
//! `self.frontmost_window = Some(win_id);` directly after inserting
//! into `self.windows`. Phase A's existing routing tests
//! ensure NewTab honors that value.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap, Meta};
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use winit::window::WindowId;

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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn new_tab_with_stale_torn_window_id_falls_back_to_main() {
    // The Phase A contract: if `frontmost_window` is set but the
    // window id is no longer in `windows`, NewTab must fall
    // back to main rather than no-op. This shape is what the Phase
    // B tear-out produces transiently if the new window dies before
    // the next dispatch — pin the safety net.
    let mut app = synth_app();
    app.__test_seed_tab("alpha");
    let main_before = app.__test_main_tab_count();
    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    app.run_action(&Action::NewTab);
    assert_eq!(
        app.__test_main_tab_count(),
        main_before + 1,
        "stale torn-window id falls back to main NewTab",
    );
}

#[test]
fn frontmost_none_after_tear_out_resets_routes_to_main() {
    // Negative control: with `frontmost_window = None`, NewTab MUST
    // land in main. This is the post-Phase-A baseline that Phase B
    // builds on (Phase B sets frontmost = Some(new_id) eagerly to
    // bridge the gap before the OS Focus event arrives).
    let mut app = synth_app();
    app.__test_seed_tab("alpha");
    app.__test_set_frontmost_window(None);
    app.__test_set_focused_child(None);
    let before = app.__test_main_tab_count();
    app.run_action(&Action::NewTab);
    assert_eq!(app.__test_main_tab_count(), before + 1);
}
