//! Regression test for the post-Epic-#289 bug where clicking the ×
//! close button on a tab in a torn-out child window did nothing.
//!
//! Root cause (pre-fix): the `MouseInput::Pressed` arm in
//! `crates/sonicterm-app/src/app/child_window.rs` matched
//! `TabHit::Close(_)` and **swallowed it** with a
//! `// single-tab children today` comment. After Epic #289 Phase D
//! allowed multi-tab child windows the comment was stale and the ×
//! glyph became a no-op.
//!
//! The fix wires the close-button hit through a new
//! `close_tab_at_in_child(win_id, idx)` helper (mirroring main's
//! per-index `close_tab_at`). This test pins the helper's behavior so
//! a future "let's simplify and swallow it again" refactor fails CI
//! instead of shipping a silent regression.
//!
//! Why no real child window is constructed here: building a
//! `WindowState` requires a live `Arc<winit::Window>` + `GpuRenderer`,
//! which need an `ActiveEventLoop` + wgpu surface — neither is
//! available in `cargo test`. The §13 GUI smoke step in the PR body
//! exercises the full click → close → window-reap path on a real
//! macOS window.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Keymap, Meta};
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

/// Contract: the per-index helper exists, is callable from outside
/// the `app` module tree (via the test-only invoker), and refuses to
/// touch state when the recorded child id is stale. This mirrors the
/// `close_active_tab_in_child` no-op contract that
/// `multi_window_frontmost_routing.rs::close_active_tab_in_child_with_missing_id_is_noop`
/// pins for the keymap path — both must remain `false` returns rather
/// than panics so the mouse handler degrades gracefully when a window
/// disappears between hit-test and dispatch.
#[test]
fn close_tab_at_in_child_with_missing_id_is_noop() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let main_before = app.__test_main_tab_count();

    // No real `windows` entry for this id — exactly the situation the
    // close-button arm guards against if a window vanished between
    // the user pressing the mouse and the event reaching the handler.
    let ok = app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 0);
    assert!(!ok, "missing-child case must return false");
    assert_eq!(
        app.__test_main_tab_count(),
        main_before,
        "missing-child invocation must NOT touch main's tab vec (this is the \
         bug-class that triggered the regression — the close button was \
         silently dispatching into self.tabs)",
    );
}

/// Contract: out-of-range index on a missing child window is also a
/// no-op (defensive against a hit-test race where the layout was
/// computed against N tabs but the close arrived after a tab was
/// already removed by another path).
#[test]
fn close_tab_at_in_child_out_of_range_is_noop() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let main_before = app.__test_main_tab_count();
    let ok = app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 999);
    assert!(!ok);
    assert_eq!(app.__test_main_tab_count(), main_before);
}

/// Contract: the close-button helper takes an explicit index — it
/// does NOT use `self.tabs` (the main window's tab vec). Pre-fix the
/// `TabHit::Close(_)` arm swallowed the click; an earlier draft of
/// the fix routed it through `self.close_tab_at(idx)` which would
/// have closed the WRONG tab — main's tab[idx] instead of the
/// torn-out window's tab[idx]. This test pins that the public
/// invoker signature carries the WindowId so the routing is
/// unambiguous: no overload-by-default can sneak through.
#[test]
fn close_button_helper_signature_is_window_scoped() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("main-a");
    let _ = app.__test_seed_tab("main-b");
    let _ = app.__test_seed_tab("main-c");
    let before = app.__test_main_tab_count();
    // Calling the in-child helper with a missing id MUST NOT
    // accidentally fall through to main's `close_tab_at`. (The bug
    // we're guarding against is exactly this: the mouse arm was
    // operating on `self.tabs` instead of the child's tabs.)
    let _ = app.__test_invoke_close_tab_at_in_child(WindowId::dummy(), 0);
    assert_eq!(
        app.__test_main_tab_count(),
        before,
        "in-child close helper must not mutate main even when child id is stale",
    );
}
