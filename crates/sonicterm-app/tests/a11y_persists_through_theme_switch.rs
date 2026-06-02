use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{AccessibilityConfig, Config},
    keymap::{Action, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

fn hex(value: &str) -> Hex {
    Hex(value.to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex("#010101"),
        red: hex("#020202"),
        green: hex("#030303"),
        yellow: hex("#040404"),
        blue: hex("#050505"),
        magenta: hex("#060606"),
        cyan: hex("#070707"),
        white: hex("#080808"),
    }
}

fn synth_theme(name: &str) -> Theme {
    Theme {
        name: name.to_string(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex("#111111"),
            foreground: hex("#eeeeee"),
            cursor: hex("#222222"),
            cursor_text: hex("#dddddd"),
            selection_bg: hex("#333333"),
            selection_fg: hex("#cccccc"),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex("#444444"),
                active_bg: hex("#555555"),
                active_fg: hex("#bbbbbb"),
                inactive_bg: hex("#666666"),
                inactive_fg: hex("#aaaaaa"),
                hover_bg: hex("#777777"),
                hover_fg: hex("#999999"),
                close_button_fg: hex("#888888"),
            },
        },
    }
}

fn empty_keymap() -> Keymap {
    Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() }
}

#[test]
fn high_contrast_survives_direct_theme_switch() {
    let config = Config {
        theme: "baseline".to_string(),
        accessibility: AccessibilityConfig {
            high_contrast: true,
            ..AccessibilityConfig::default()
        },
        ..Config::default()
    };
    let mut app = App::new(synth_theme("baseline"), config, empty_keymap());
    app.set_theme_loader_for_test(Box::new(|name: &str| Ok(synth_theme(name))));

    app.run_action(&Action::ApplyTheme("tokyo-night".to_string()));

    let theme = app.theme_for_test();
    assert_eq!(theme.name, "tokyo-night");
    assert_eq!(theme.colors.foreground, hex("#ffffff"));
    assert_eq!(theme.colors.background, hex("#000000"));
}
