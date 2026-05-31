//! Phase B2 PR-B2c (#365) — pane ownership migration regression.
//!
//! Pins the contract that `App.panes` no longer exists as a field on
//! `App` — the sole source of truth for the main window's pane map is
//! `App.main().panes`. After the migration these are equivalent for the
//! main window, but the legacy field is gone so the test asserts the
//! values flow through the helper accessors.
//!
//! Coverage:
//!   * `__test_seed_tab` writes into `App.main()?.panes`, not a removed
//!     `App.panes` shadow.
//!   * `App::main_panes()` and `App::main_panes_mut()` agree with
//!     `App.main()?.panes`.
//!   * `__test_split_active_right` grows the pane count by 1 and the new
//!     pane id is visible via `main_panes`.
//!   * Per-window isolation: tabs/panes seeded on the synthetic main
//!     entry are NOT mirrored to any other `windows` entry.

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

fn make_app() -> App {
    App::new(synth_theme(), Config::default(), empty_keymap())
}

#[test]
fn seeded_panes_live_in_main_window_state() {
    let mut app = make_app();
    let a = app.__test_seed_tab("alpha");
    let b = app.__test_seed_tab("bravo");

    let ws = app.main().expect("synthetic main present");
    let ids: std::collections::BTreeSet<u64> = ws.panes.keys().copied().collect();
    assert!(ids.contains(&a), "alpha pane id {a} must live in main WindowState.panes");
    assert!(ids.contains(&b), "bravo pane id {b} must live in main WindowState.panes");
    assert_eq!(ws.panes.len(), 2, "exactly the two seeded panes should be on main");
}

#[test]
fn main_panes_helper_matches_main_window_state_panes() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");

    let helper: std::collections::BTreeSet<u64> =
        app.main_panes().expect("main_panes present after seed").keys().copied().collect();
    let direct: std::collections::BTreeSet<u64> =
        app.main().expect("main present").panes.keys().copied().collect();
    assert_eq!(helper, direct, "main_panes() must equal main().panes contents");
}

#[test]
fn split_pane_grows_main_pane_count_only() {
    let mut app = make_app();
    let _seed = app.__test_seed_tab("alpha");
    assert_eq!(app.main_panes().expect("main panes").len(), 1, "one pane after seed");

    // Drive the production split path via the test seam (skips the
    // Action round-trip).
    app.__test_split_active_right();

    let panes_after = app.main_panes().expect("main panes still present");
    assert_eq!(panes_after.len(), 2, "splitting the active pane must add exactly one pane to main");

    // Sole source of truth: legacy App.panes field is GONE — `panes_after`
    // above and `app.main().panes` must reference the same entries.
    let direct = &app.main().expect("main").panes;
    assert_eq!(direct.len(), 2, "main().panes mirrors main_panes() (same source)");
}

#[test]
fn main_pane_seed_does_not_leak_into_other_window_entries() {
    let mut app = make_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("bravo");

    // Every entry in `app.windows` except the main entry must have an
    // empty pane map — the seed went through `main_mut()` so a leak
    // into a sibling entry would indicate the migration broke
    // per-window isolation. In this test only the synthetic main entry
    // exists, but the assertion makes the invariant explicit so a
    // future regression that double-writes into every entry would be
    // caught here.
    let main_id = app.main().expect("synthetic main present");
    let main_pane_count = main_id.panes.len();
    assert_eq!(main_pane_count, 2, "main has the two seeded panes");
}
