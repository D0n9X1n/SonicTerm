use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Action, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

fn drain(rx: &crossbeam_channel::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        out.extend_from_slice(&chunk);
    }
    out
}

fn hex(value: &str) -> Hex {
    Hex(value.to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex("#000000"),
        red: hex("#111111"),
        green: hex("#222222"),
        yellow: hex("#333333"),
        blue: hex("#444444"),
        magenta: hex("#555555"),
        cyan: hex("#666666"),
        white: hex("#777777"),
    }
}

fn theme(name: &str, foreground: &str, background: &str, cursor: &str) -> Theme {
    Theme {
        name: name.to_string(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(background),
            foreground: hex(foreground),
            cursor: hex(cursor),
            cursor_text: hex("#101010"),
            selection_bg: hex("#202020"),
            selection_fg: hex("#303030"),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex("#404040"),
                active_bg: hex("#505050"),
                active_fg: hex("#606060"),
                inactive_bg: hex("#707070"),
                inactive_fg: hex("#808080"),
                hover_bg: hex("#909090"),
                hover_fg: hex("#a0a0a0"),
                close_button_fg: hex("#b0b0b0"),
            },
        },
    }
}

fn empty_keymap() -> Keymap {
    Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() }
}

#[test]
fn theme_reload_propagates_osc_11_background_to_existing_pane_parser() {
    let config = Config { theme: "baseline".to_string(), ..Config::default() };
    let mut app =
        App::new(theme("baseline", "#eeeeee", "#141617", "#dddddd"), config, empty_keymap());
    let (pane_id, rx) = app.__test_seed_tab_with_reply("shell");
    app.__test_seed_pane_theme_colors(pane_id);

    assert!(app.__test_advance_pane_parser(pane_id, b"\x1b]11;?\x1b\\"));
    assert_eq!(drain(&rx), b"\x1b]11;rgb:1414/1616/1717\x1b\\");

    app.set_theme_loader_for_test(Box::new(|name: &str| {
        Ok(theme(name, "#f8f8f2", "#282a36", "#ff79c6"))
    }));
    app.run_action(&Action::ApplyTheme("dracula".to_string()));

    assert!(app.__test_advance_pane_parser(pane_id, b"\x1b]11;?\x1b\\"));
    assert_eq!(drain(&rx), b"\x1b]11;rgb:2828/2a2a/3636\x1b\\");
}
