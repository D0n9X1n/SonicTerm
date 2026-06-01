//! Regression test: the close-button (×) hit-test path in a torn-out
//! child window MUST route to that window's own `tabs` vec — not to
//! main's `self.tabs`. This is the routing-half of the post-Epic-#289
//! bug; `tearout_close_button_works.rs` covers the helper-existence /
//! signature half.
//!
//! Why this test stops at the routing primitive: constructing two
//! real child windows requires a live `ActiveEventLoop` + two wgpu
//! surfaces, neither available in a unit-test binary. The §13 GUI
//! smoke in the PR body exercises the full "2 windows, click × on
//! the back one's tab[0]" scenario on a real macOS desktop.

use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
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

/// Frontmost-routing isolation: if a child window's × is clicked
/// (the event arrives carrying THAT window's `WindowId`), the close
/// helper must dispatch against THAT window — even if a different
/// window happens to be recorded as `frontmost`. The mouse handler
/// reaches `close_tab_at_in_child` with the `event_window_id`, not
/// `self.frontmost_window`, and the helper is window-scoped via its
/// signature — so this test pins that an unrelated frontmost
/// recording cannot misroute the close.
#[test]
fn close_in_child_does_not_consult_frontmost() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("main-only");
    // Pretend some other window is frontmost; the close arm must
    // ignore this and use the event's window id.
    app.__test_set_frontmost_window(Some(WindowId::dummy()));
    let main_before = app.__test_main_tab_count();
    // Call helper with a *different* dummy id — neither matches a
    // real child, so it's a no-op. Crucially, main must not be
    // touched as a "fallback".
    let ok = app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 0);
    assert!(!ok);
    assert_eq!(
        app.__test_main_tab_count(),
        main_before,
        "close-in-child must NEVER fall through to main when child id is unknown",
    );
}

/// Companion: the keymap-driven `close_active_tab_in_child` arm
/// already pins the same routing contract, but with the per-index
/// (mouse-driven) helper added we want explicit coverage that BOTH
/// dispatchers share the no-fallback-to-main behavior. If a future
/// refactor consolidates them, this test catches the mistake of
/// adding an `else { self.close_tab_at(idx) }` branch.
#[test]
fn both_in_child_close_paths_no_op_on_missing_id() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("a");
    let _ = app.__test_seed_tab("b");
    let before = app.__test_main_tab_count();

    assert!(!app.__test_invoke_close_active_tab_in_child(WindowId::dummy()));
    assert!(!app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 0));
    assert!(!app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 1));

    assert_eq!(
        app.__test_main_tab_count(),
        before,
        "neither in-child close path may mutate main when the child id is stale",
    );
}
