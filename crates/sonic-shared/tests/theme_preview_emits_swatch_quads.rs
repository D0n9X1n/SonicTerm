use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::layout::Category;
use sonic_shared::prefs::{Control, PrefsState};
use sonic_shared::prefs_renderer::build_draw_list;

fn test_theme() -> Theme {
    let h = |s: &str| Hex(s.to_string());
    let ansi = AnsiColors {
        black: h("#000000"),
        red: h("#800000"),
        green: h("#008000"),
        yellow: h("#808000"),
        blue: h("#000080"),
        magenta: h("#800080"),
        cyan: h("#008080"),
        white: h("#c0c0c0"),
    };
    let bright = AnsiColors {
        black: h("#808080"),
        red: h("#ff0000"),
        green: h("#00ff00"),
        yellow: h("#ffff00"),
        blue: h("#0000ff"),
        magenta: h("#ff00ff"),
        cyan: h("#00ffff"),
        white: h("#ffffff"),
    };
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: h("#1d2021"),
            foreground: h("#ebdbb2"),
            cursor: h("#ebdbb2"),
            cursor_text: h("#1d2021"),
            selection_bg: h("#504945"),
            selection_fg: h("#ebdbb2"),
            ansi,
            bright,
            tab: TabColors {
                bar_bg: h("#1d2021"),
                active_bg: h("#282828"),
                active_fg: h("#fabd2f"),
                inactive_bg: h("#1d2021"),
                inactive_fg: h("#a89984"),
                hover_bg: h("#3c3836"),
                hover_fg: h("#ebdbb2"),
                close_button_fg: h("#fb4934"),
            },
        },
    }
}

#[test]
fn theme_preview_emits_swatch_quads() {
    let theme = test_theme();
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    state.set_category(Category::Theme);

    let theme_dropdown_id = state
        .controls
        .iter()
        .find_map(|c| if let Control::Dropdown(d) = c { Some(d.id) } else { None })
        .expect("Theme section has dropdown");
    state.toggle_dropdown(theme_dropdown_id);

    let (row_x, row_w, row_h, options_len) = state
        .controls
        .iter()
        .find_map(|c| {
            if let Control::Dropdown(d) = c {
                Some((d.rect.x, d.rect.w, d.rect.h, d.options.len()))
            } else {
                None
            }
        })
        .expect("Theme dropdown exists");

    let draw = build_draw_list(&state, &theme);

    for row_idx in 0..options_len {
        let row_y = state.layout.control_slot(0).y + row_h * (row_idx as f32 + 1.0);
        let swatch_x = row_x + row_w - 146.0;
        let swatch_quads = draw
            .popover_quads
            .iter()
            .filter(|q| {
                q.rect.x >= swatch_x - 0.5
                    && q.rect.x <= row_x + row_w
                    && q.rect.y >= row_y + 3.5
                    && q.rect.y <= row_y + row_h - 0.5
            })
            .count();
        assert!(
            swatch_quads >= 9,
            "row {row_idx} should emit bg + 8 ANSI tile quads, got {swatch_quads}"
        );
    }

    let sample_count = draw.popover_texts.iter().filter(|t| t.text == "Aa").count();
    assert_eq!(sample_count, options_len, "each theme row emits an Aa foreground sample");
}
