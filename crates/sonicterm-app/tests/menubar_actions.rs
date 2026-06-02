//! Integration tests for the menubar-driven actions added by
//! `feat: native macOS menubar` — `IncreaseFontSize`,
//! `DecreaseFontSize`, `ResetFontSize`, `ApplyTheme(String)`,
//! `ToggleTabBar`.
//!
//! We exercise the `App::run_action` arms directly (no live wgpu
//! surface). Renderer side effects are already covered by the
//! existing `font_live_reload.rs` + config-watch tests.

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, FontConfig},
    keymap::{Action, Keymap, Meta},
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

fn synth_theme(name: &str) -> Theme {
    Theme {
        name: name.to_string(),
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
    App::new(synth_theme("baseline"), Config::default(), empty_keymap())
}

#[test]
fn font_size_increase_then_decrease_round_trips() {
    let mut app = make_app();
    let start = app.font_size_for_test();
    app.run_action(&Action::IncreaseFontSize);
    assert!((app.font_size_for_test() - (start + 1.0)).abs() < f32::EPSILON);
    app.run_action(&Action::DecreaseFontSize);
    assert!((app.font_size_for_test() - start).abs() < f32::EPSILON);
}

#[test]
fn font_size_clamps_at_lower_bound() {
    let mut app = make_app();
    for _ in 0..200 {
        app.run_action(&Action::DecreaseFontSize);
    }
    assert!((app.font_size_for_test() - 8.0).abs() < f32::EPSILON);
}

#[test]
fn font_size_clamps_at_upper_bound() {
    let mut app = make_app();
    for _ in 0..200 {
        app.run_action(&Action::IncreaseFontSize);
    }
    assert!((app.font_size_for_test() - 48.0).abs() < f32::EPSILON);
}

#[test]
fn reset_font_size_returns_to_default() {
    let mut app = make_app();
    app.run_action(&Action::IncreaseFontSize);
    app.run_action(&Action::IncreaseFontSize);
    app.run_action(&Action::IncreaseFontSize);
    app.run_action(&Action::ResetFontSize);
    let default = FontConfig::default().size;
    assert!((app.font_size_for_test() - default).abs() < f32::EPSILON);
}

#[test]
fn apply_theme_swaps_live_theme_and_persists_to_config() {
    let mut app = make_app();
    app.set_theme_loader_for_test(Box::new(|name: &str| Ok(synth_theme(name))));

    assert_eq!(app.theme_name_for_test(), "baseline");
    assert_ne!(app.config_for_test().theme.as_str(), "dracula");

    app.run_action(&Action::ApplyTheme("dracula".to_string()));
    assert_eq!(app.theme_name_for_test(), "dracula");
    assert_eq!(app.config_for_test().theme.as_str(), "dracula");
}

#[test]
fn apply_theme_no_loader_is_safe_noop() {
    let mut app = make_app();
    app.run_action(&Action::ApplyTheme("nord".to_string()));
    assert_eq!(app.theme_name_for_test(), "baseline");
}

#[test]
fn toggle_tab_bar_flips_visibility_flag() {
    let mut app = make_app();
    assert!(app.tab_bar_visible());
    app.run_action(&Action::ToggleTabBar);
    assert!(!app.tab_bar_visible());
    app.run_action(&Action::ToggleTabBar);
    assert!(app.tab_bar_visible());
}

/// Regression for Haiku review on PR #66: toggling the bar must also
/// drop the renderer's top inset to 0 so the grid recovers the row.
/// We validate the pure helper that backs `GpuRenderer::top_inset` —
/// any production change that severs the wiring will also have to
/// rewrite this assertion.
#[test]
fn tab_bar_top_inset_drops_bar_band_when_hidden() {
    use sonicterm_ui::tabbar_view::tab_bar_top_inset;
    use sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT;
    let pad = 4.0_f32;
    let shown = tab_bar_top_inset(true, pad);
    let hidden = tab_bar_top_inset(false, pad);
    assert_eq!(shown, TAB_BAR_HEIGHT + pad);
    // Per-side padding parity (PR feat/per-side-padding-parity): top
    // padding is reserved above the grid even when the tab bar is
    // hidden, matching `window_padding.top` in WezTerm.
    assert_eq!(hidden, pad);
}
