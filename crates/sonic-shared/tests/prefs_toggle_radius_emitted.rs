//! Regression for issue #173 slice-2c (Toggle-only wire-up):
//!
//! The preferences toggles used to emit raw sharp `QuadCmd` tracks
//! (radius 0). The slice-2c redesign requires the renderer to route
//! every Toggle through the new primitive so the track paints as a
//! rounded pill (`radius_px == TOGGLE_H / 2`) and the sliding thumb
//! paints as a circle (`radius_px == TOGGLE_KNOB / 2`).
//!
//! This pins both invariants. If a future refactor accidentally drops
//! the radius (e.g. by re-introducing `QuadCmd::sharp` for the toggle
//! track) the assertions fail.

use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::controls::Control;
use sonic_shared::prefs::layout::{Category, TOGGLE_H, TOGGLE_KNOB, TOGGLE_W};
use sonic_shared::prefs::PrefsState;
use sonic_shared::prefs_renderer::build_draw_list;

fn test_theme() -> Theme {
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
            background: Hex("#1d2021".to_string()),
            foreground: Hex("#ebdbb2".to_string()),
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

/// Walk every category until we find one whose form contains at least
/// one [`Control::Toggle`]; sets it as the active category and returns
/// the state. Panics if no toggle exists anywhere, which would itself
/// be a regression.
fn state_with_toggle() -> (PrefsState, Theme) {
    let theme = test_theme();
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    for cat in [
        Category::General,
        Category::Appearance,
        Category::Behavior,
        Category::Keymap,
        Category::Font,
    ] {
        state.set_category(cat);
        if state.controls.iter().any(|c| matches!(c, Control::Toggle(_))) {
            return (state, theme);
        }
    }
    panic!("no Toggle found in any prefs category");
}

#[test]
fn prefs_toggle_track_emits_rounded_pill() {
    let (state, theme) = state_with_toggle();
    let draw = build_draw_list(&state, &theme);

    // Track radius is half the track height (a true pill). For the
    // 24px TOGGLE_H this is 12.0.
    let expected_radius = TOGGLE_H / 2.0;
    let track_quad = draw
        .quads
        .iter()
        .find(|q| {
            (q.rect.w - TOGGLE_W).abs() < 0.01
                && (q.rect.h - TOGGLE_H).abs() < 0.01
                && (q.radius_px - expected_radius).abs() < 1e-3
        })
        .unwrap_or_else(|| {
            panic!(
                "expected a {TOGGLE_W}×{TOGGLE_H} rounded track quad with radius={expected_radius}; \
                 quads: {:?}",
                draw.quads
                    .iter()
                    .filter(|q| (q.rect.w - TOGGLE_W).abs() < 0.01)
                    .collect::<Vec<_>>()
            )
        });

    assert!(
        track_quad.radius_px > 0.0,
        "toggle track must be a rounded pill, not a sharp rect; got radius_px = {}",
        track_quad.radius_px
    );
}

#[test]
fn prefs_toggle_knob_emits_rounded_circle() {
    let (state, theme) = state_with_toggle();
    let draw = build_draw_list(&state, &theme);

    let expected_radius = TOGGLE_KNOB / 2.0;
    let knob_quad = draw.quads.iter().find(|q| {
        (q.rect.w - TOGGLE_KNOB).abs() < 0.01
            && (q.rect.h - TOGGLE_KNOB).abs() < 0.01
            && (q.radius_px - expected_radius).abs() < 1e-3
    });

    assert!(
        knob_quad.is_some(),
        "expected a {TOGGLE_KNOB}×{TOGGLE_KNOB} circular knob quad with radius={expected_radius}"
    );
}
