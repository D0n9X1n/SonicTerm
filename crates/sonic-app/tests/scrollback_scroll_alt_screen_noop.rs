//! #412 — wheel events on a pane in alt-screen mode must NOT shift
//! `viewport_top_abs`. Full-screen TUIs (vim/htop/fzf) own their own
//! scroll semantics; the host must not synthesize a viewport shift
//! behind their back.

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

#[test]
fn scroll_pane_in_alt_screen_is_noop() {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: vec![] };
    let mut app = App::new(synth_theme(), Config::default(), keymap);
    let pane_id = app.__test_seed_tab("only");
    let _ = app.__test_grow_pane_scrollback(pane_id, 200);
    // Enter alt-screen (DECSET 1049).
    app.__test_advance_pane_parser(pane_id, b"\x1b[?1049h");
    let before = app.__test_pane_viewport_top_abs(pane_id).unwrap();
    app.scroll_pane(pane_id, -10);
    let after = app.__test_pane_viewport_top_abs(pane_id).unwrap();
    assert_eq!(before, after, "alt-screen scroll must be a no-op");
}
