//! Regression for issue #173 slice-2 (Button-only wire-up):
//!
//! The preferences footer Apply / Cancel buttons used to emit raw
//! sharp `QuadCmd { rect, color }` instances (radius 0). The slice-2
//! redesign requires the renderer to route them through the new
//! [`Button`] primitive so they paint as pill-shaped, rounded-corner
//! quads with `radius_px == BUTTON_RADIUS`, and so the text's
//! horizontal anchor matches `button.text_center().0`.
//!
//! This test pins both invariants. If a future refactor accidentally
//! drops the radius (e.g. by re-introducing `QuadCmd::sharp` for the
//! buttons) or shifts the rendered rect away from the button's
//! actual `Rect`, the assertions fail.

use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::PrefsState;
use sonic_shared::prefs_renderer::build_draw_list;

// Layout constant lives in `sonic_ui::prefs::layout` and is the
// single source of truth for the GPU side; importing it here keeps
// the test honest if someone bumps the value.
use sonic_shared::prefs::layout::BUTTON_RADIUS;

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

#[test]
fn prefs_apply_button_emits_rounded_quad_with_centered_text() {
    let theme = test_theme();
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    // Force dirty so the Apply button paints in its enabled state
    // (the disabled-state palette is exercised by a sibling test).
    state.dirty = true;

    let draw = build_draw_list(&state, &theme);

    let apply_rect = state.apply_button.rect;
    let apply_quad = draw
        .quads
        .iter()
        .find(|q| {
            (q.rect.x - apply_rect.x).abs() < 0.01
                && (q.rect.y - apply_rect.y).abs() < 0.01
                && (q.rect.w - apply_rect.w).abs() < 0.01
                && (q.rect.h - apply_rect.h).abs() < 0.01
        })
        .expect("apply button quad with matching rect");

    assert!(
        (apply_quad.radius_px - BUTTON_RADIUS).abs() < 1e-4,
        "apply button radius_px should equal BUTTON_RADIUS ({}), got {}",
        BUTTON_RADIUS,
        apply_quad.radius_px,
    );

    // Text-center invariant: Button::text_center is the renderer's
    // anchor for the centered "Apply" label. It must equal the
    // geometric center of the button rect.
    let (cx, cy) = state.apply_button.text_center();
    assert!((cx - (apply_rect.x + apply_rect.w / 2.0)).abs() < 1e-4, "text x-center off");
    assert!((cy - (apply_rect.y + apply_rect.h / 2.0)).abs() < 1e-4, "text y-center off");
}

#[test]
fn prefs_cancel_button_emits_rounded_quad() {
    let theme = test_theme();
    let state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());

    let draw = build_draw_list(&state, &theme);

    let cancel_rect = state.cancel_button.rect;
    let cancel_quad = draw
        .quads
        .iter()
        .find(|q| {
            (q.rect.x - cancel_rect.x).abs() < 0.01
                && (q.rect.y - cancel_rect.y).abs() < 0.01
                && (q.rect.w - cancel_rect.w).abs() < 0.01
                && (q.rect.h - cancel_rect.h).abs() < 0.01
        })
        .expect("cancel button quad with matching rect");

    assert!(
        (cancel_quad.radius_px - BUTTON_RADIUS).abs() < 1e-4,
        "cancel button radius_px should equal BUTTON_RADIUS ({}), got {}",
        BUTTON_RADIUS,
        cancel_quad.radius_px,
    );
}
