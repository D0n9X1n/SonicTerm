use sonic_cfg::config::Config;
use sonic_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_ui::prefs::{Category, PrefsHit, PrefsState};

fn theme() -> Theme {
    let h = || Hex("#7aa2f7".to_string());
    let ansi = || AnsiColors {
        black: h(),
        red: h(),
        green: h(),
        yellow: h(),
        blue: h(),
        magenta: h(),
        cyan: h(),
        white: h(),
    };
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: h(),
            foreground: h(),
            cursor: h(),
            cursor_text: h(),
            selection_bg: h(),
            selection_fg: h(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: h(),
                active_bg: h(),
                active_fg: h(),
                inactive_bg: h(),
                inactive_fg: h(),
                hover_bg: h(),
                hover_fg: h(),
                close_button_fg: h(),
            },
        },
    }
}

#[test]
fn reset_link_hit_resets_font_section_to_config_default() {
    let mut cfg = Config::default();
    cfg.font.family = "Fira Code".into();
    cfg.font.size = 24.0;
    cfg.font.line_height = 1.4;

    let mut state =
        PrefsState::new(cfg, std::env::temp_dir().join("sonic-reset-font.toml"), theme());
    state.set_category(Category::Font);
    let r = state.layout.reset_link;
    assert_eq!(state.classify_click(r.x + 2.0, r.y + 2.0), Some(PrefsHit::ResetSection));

    state.reset_active_section_to_default();
    assert_eq!(state.config.font, Config::default().font);
    assert!(state.is_dirty());
}
