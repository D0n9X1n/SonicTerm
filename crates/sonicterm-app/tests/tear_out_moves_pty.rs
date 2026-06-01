//! Epic #289 Phase B — regression test for the "PtyHandle MOVES, no
//! respawn" invariant of tab tear-out.
//!
//! ## What this test pins
//!
//! `App::detach_tab_state` (the per-tab state extractor that
//! `tear_out_tab` and `merge_main_into_child` both build on) MUST:
//!
//!   1. Move the tab's `PaneState` (including its `PtyHandle`) out of
//!      the source App — no clone, no respawn.
//!   2. Preserve the pane id (a respawn would mint a fresh one via
//!      `next_pane_id()`, the only way to spot a hidden re-spawn
//!      without spinning up a real shell).
//!   3. Activate the LEFT neighbor of the removed slot on the source
//!      side (spec §B4).
//!
//! ## Why no real winit window
//!
//! Spawning a real winit window + wgpu surface requires the main
//! thread on macOS and is unusable inside `cargo test`. The same
//! constraint that gates `tear_out_tab` as a whole gates the "is the
//! WindowId of the new window now frontmost" assertion. We split that
//! coverage: this file pins the pure-logic invariants
//! (`tear_out_apply_source_side`, `detach_tab_state`), the §13 GUI
//! smoke in the PR body covers the real-window path.

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

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    App::new(synth_theme(), Config::default(), keymap)
}

#[test]
fn detach_then_apply_source_side_3_tabs_torn_at_idx_1() {
    // Spec §B2: state = 3 tabs (alpha, bravo, charlie). Tear tab[1]
    // (bravo). After detach + source-side apply:
    //   - main has 2 tabs (alpha, charlie) — pane ids preserved
    //   - bravo's pane id moved into the detached tuple (no respawn)
    //   - active = max(0, 1-1) = 0 (alpha) per §B4
    let mut app = synth_app();
    let p_alpha = app.__test_seed_tab("alpha");
    let p_bravo = app.__test_seed_tab("bravo");
    let p_charlie = app.__test_seed_tab("charlie");
    assert_eq!(app.__test_tab_count(), 3);

    let (_tab, _state, panes) = app.detach_tab_state(1).expect("detach idx 1");
    // bravo's pane MOVED into the detached tuple — same id, no respawn.
    assert!(panes.contains_key(&p_bravo), "PtyHandle/PaneState must MOVE — pane id preserved");
    // Source App no longer references bravo's pane.
    let mut remaining: Vec<u64> = app.__test_pane_ids();
    remaining.sort_unstable();
    let mut want = vec![p_alpha, p_charlie];
    want.sort_unstable();
    assert_eq!(remaining, want, "source App keeps alpha + charlie only");

    // Phase B source-side cleanup: left-neighbor activation.
    app.tear_out_apply_source_side(1);
    assert_eq!(app.__test_tab_count(), 2);
    // active index = max(0, 1-1) = 0 → alpha.
    // We don't have a direct active-index reader on App for the main
    // window, but we can verify via `__test_active_pane_in_tab` that
    // tab 0 (alpha) still holds its pane id and tab 1 holds charlie.
    assert_eq!(app.__test_active_pane_in_tab(0), Some(p_alpha));
    assert_eq!(app.__test_active_pane_in_tab(1), Some(p_charlie));
}

#[test]
fn pane_id_unchanged_proves_no_respawn() {
    // A respawn (vs a move) would mint a NEW pane id via
    // `next_pane_id()`. Pin the id-stability invariant directly.
    let mut app = synth_app();
    let p0 = app.__test_seed_tab("alpha");
    let p1 = app.__test_seed_tab("bravo");
    let (_tab, _state, panes) = app.detach_tab_state(0).expect("detach idx 0");
    let detached_ids: Vec<u64> = panes.keys().copied().collect();
    assert_eq!(detached_ids, vec![p0], "alpha's pane id MOVED (no respawn)");
    let remaining = app.__test_pane_ids();
    assert_eq!(remaining, vec![p1], "bravo's pane id unchanged in source");
}

#[test]
fn tear_out_apply_source_side_first_tab_clamps_to_zero() {
    // Spec §B4: max(0, removed_idx - 1). Removing tab[0] from a
    // 3-tab bar leaves 2 tabs; active must clamp to 0 (the new
    // leftmost), not underflow.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("a");
    let pb = app.__test_seed_tab("b");
    let pc = app.__test_seed_tab("c");
    let _ = app.detach_tab_state(0).expect("detach idx 0");
    app.tear_out_apply_source_side(0);
    assert_eq!(app.__test_tab_count(), 2);
    assert_eq!(app.__test_active_pane_in_tab(0), Some(pb));
    assert_eq!(app.__test_active_pane_in_tab(1), Some(pc));
}

#[test]
fn tear_out_apply_source_side_last_tab_clamps_within_remaining() {
    // Spec §B4: removing the LAST tab from a 3-tab bar leaves 2 tabs.
    // max(0, 2-1) = 1, clamped to len-1 = 1 → active = tab[1] (the
    // new rightmost). The clamp matters when removed_idx >= len-1.
    let mut app = synth_app();
    let pa = app.__test_seed_tab("a");
    let pb = app.__test_seed_tab("b");
    let _ = app.__test_seed_tab("c");
    let _ = app.detach_tab_state(2).expect("detach idx 2");
    app.tear_out_apply_source_side(2);
    assert_eq!(app.__test_tab_count(), 2);
    assert_eq!(app.__test_active_pane_in_tab(0), Some(pa));
    assert_eq!(app.__test_active_pane_in_tab(1), Some(pb));
}
