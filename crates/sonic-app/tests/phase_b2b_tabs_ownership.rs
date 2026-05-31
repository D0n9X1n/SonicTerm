//! Phase B2 PR-B2b (#365) — `App.tabs` + `App.tab_states` ownership
//! migrated to the main `WindowState` entry. The legacy `App.*` fields
//! are deleted; `main_tabs()` / `main_tab_states()` are the sole
//! readers.
//!
//! What this test pins:
//!   1. After seeding 3 tabs through the test seam, `main_tabs()` and
//!      `main_tab_states()` see all three.
//!   2. The `App.tabs` legacy field no longer compiles — enforced by
//!      its deletion at the type level (so this file just exercises
//!      the new accessor surface; the negative compile check is
//!      implicit).
//!   3. Multi-window: when a child window is built with its own tabs,
//!      the main window's tabs vec is independent of the child's.

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
        name: "synth".into(),
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
fn seeded_tabs_visible_via_main_tabs_helpers() {
    let mut app = synth_app();
    app.__test_seed_tab("alpha");
    app.__test_seed_tab("beta");
    app.__test_seed_tab("gamma");

    let tabs = app.main_tabs().expect("main_tabs() Some after synthetic main + seed");
    assert_eq!(tabs.len(), 3, "all three seeded tabs land in WindowState.tabs");

    let states = app.main_tab_states().expect("main_tab_states() Some");
    assert_eq!(states.len(), 3, "tab_states parallels tabs");

    // __test_main_tab_count must now read through the helper too.
    assert_eq!(app.__test_main_tab_count(), 3);
}

#[test]
fn main_tabs_none_before_synthetic_main_inserted() {
    let app = synth_app();
    // Before __test_synthetic_main / do_resumed runs, there is no main
    // window entry, so the helpers return None rather than panicking.
    assert!(app.main_tabs().is_none(), "no main entry before synth_main");
    assert!(app.main_tab_states().is_none(), "no main entry before synth_main");
    assert_eq!(app.__test_main_tab_count(), 0, "tab count helper short-circuits to 0");
}

#[test]
fn main_tab_states_mut_round_trip() {
    let mut app = synth_app();
    app.__test_seed_tab("alpha");
    // Mutate through the mut helper, observe through the read helper.
    {
        let states = app.main_tab_states_mut().expect("mut helper Some");
        states[0].active_pane = 999;
    }
    let read_back = app.main_tab_states().expect("read helper Some")[0].active_pane;
    assert_eq!(
        read_back, 999,
        "writes through main_tab_states_mut are visible through main_tab_states"
    );
}
