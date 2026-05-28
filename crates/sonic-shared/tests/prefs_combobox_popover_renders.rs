//! Regression for issue #173 slice-2b (Combobox wire-up):
//!
//! When a `Combobox` is open the renderer must emit popover quads into
//! the dedicated `popover_quads` / `popover_texts` layer so the second
//! render pass overlays every other widget (cards, footer, even sibling
//! combobox headers). Before slice-2b the dropdown render path
//! explicitly skipped the open-state and the popover never appeared on
//! screen (issues #166, #168). The follow-up to slice-2b (Haiku review
//! of PR #210) moved popover content out of the base `quads`/`texts`
//! vectors and into the dedicated layer to fix base-text overdrawing
//! the popover.
//!
//! Pins:
//!   1. At least one quad sits at `y == d.rect.y + d.rect.h` (i.e.
//!      directly below the closed header) in `popover_quads`.
//!   2. No base-layer quad sits in the popover band, so base text
//!      cannot overdraw the popover.
//!   3. One text entry per option lives in `popover_texts`.

use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::layout::Category;
use sonic_shared::prefs::{Control, PrefsState};
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

#[test]
fn open_combobox_popover_is_emitted_below_header() {
    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    s.set_category(Category::Theme);

    // Find the theme dropdown, open it, then re-snapshot rect + options.
    let id = s
        .controls
        .iter()
        .find_map(|c| if let Control::Dropdown(d) = c { Some(d.id) } else { None })
        .expect("Appearance has a theme dropdown");
    s.toggle_dropdown(id);

    let (rect, options): (_, Vec<String>) = match s.controls.iter().find(|c| c.id() == id).unwrap()
    {
        Control::Dropdown(d) => (d.rect, d.options.clone()),
        _ => unreachable!(),
    };
    assert!(!options.is_empty(), "theme dropdown should have at least one option");

    let dl = build_draw_list(&s, &theme);

    // 1. A popover quad sits at the row directly below the header — and
    //    it lives in the dedicated popover layer.
    let popover_top_y = rect.y + rect.h;
    let popover_quads_at_top: Vec<_> =
        dl.popover_quads.iter().filter(|q| (q.rect.y - popover_top_y).abs() < 0.01).collect();
    assert!(
        !popover_quads_at_top.is_empty(),
        "popover_quads should contain a quad at y={popover_top_y} (directly below header)",
    );

    // 2. The popover BACKDROP (the rounded surface quad spanning the
    //    full popover area) must NOT exist in the base `quads` vector.
    //    Some base controls (sibling dropdown headers in following
    //    form rows) naturally sit at the same y as the popover band —
    //    they correctly draw UNDER the popover in the new pipeline,
    //    so we only assert against the exact popover-backdrop rect.
    let popover_bottom = popover_top_y + rect.h * options.len() as f32;
    let pop_h = popover_bottom - popover_top_y;
    let leaked = dl.quads.iter().any(|q| {
        (q.rect.x - rect.x).abs() < 0.5
            && (q.rect.y - popover_top_y).abs() < 0.5
            && (q.rect.w - rect.w).abs() < 0.5
            && (q.rect.h - pop_h).abs() < 0.5
    });
    assert!(!leaked, "popover backdrop leaked into base `quads` — must live in popover_quads");

    // 3. One text entry per option lives in popover_texts.
    for opt in &options {
        assert!(
            dl.popover_texts.iter().any(|t| &t.text == opt),
            "popover_texts should emit text for option {opt:?}",
        );
    }
}

#[test]
fn closed_combobox_does_not_emit_selected_row_highlight() {
    // When a dropdown is CLOSED, the renderer must not emit the
    // `palette.bg_hover` "selected row" highlight quad below the header
    // (that quad only exists inside the popover overlay). Use the
    // palette identity to distinguish the popover quads from other
    // headers at the same y — overlapping form rows are fine, but a
    // bg_hover stripe directly below a closed dropdown is a popover
    // leak. Check BOTH layers — neither should contain the highlight.
    use sonic_shared::ui_tokens::UiPalette;

    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    s.set_category(Category::Theme);
    let rect = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) => Some(d.rect),
            _ => None,
        })
        .expect("Appearance has a theme dropdown");

    let dl = build_draw_list(&s, &theme);
    let palette = UiPalette::from_theme(&theme);
    let popover_top_y = rect.y + rect.h;

    let predicate = |q: &sonic_shared::prefs_renderer::QuadCmd| {
        (q.rect.y - popover_top_y).abs() < 0.01
            && (q.rect.x - rect.x).abs() < 0.01
            && q.color == palette.bg_hover
    };
    assert!(
        !dl.quads.iter().any(predicate),
        "closed dropdown should not emit popover selected-row highlight in base quads"
    );
    assert!(
        !dl.popover_quads.iter().any(predicate),
        "closed dropdown should not emit popover selected-row highlight in popover_quads"
    );
}
