//! Regression for issue #173 slice-2b (Combobox wire-up):
//!
//! When a `Combobox` is open the renderer must emit popover quads at
//! the END of the draw list so they overlay every other widget (cards,
//! footer, even sibling combobox headers). Before this slice the
//! dropdown render path explicitly skipped the open-state and the
//! popover never appeared on screen (issues #166, #168).
//!
//! Pins:
//!   1. At least one quad sits at `y == d.rect.y + d.rect.h` (i.e.
//!      directly below the closed header) when the dropdown is open.
//!   2. The popover quads land AFTER every non-popover quad in the
//!      draw list so the z-order is correct.
//!   3. One text entry is emitted per option, with the option text
//!      matching the `Dropdown.options` vector.

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
    s.set_category(Category::Appearance);

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

    // 1. A popover quad sits at the row directly below the header.
    let popover_top_y = rect.y + rect.h;
    let popover_quads: Vec<_> =
        dl.quads.iter().filter(|q| (q.rect.y - popover_top_y).abs() < 0.01).collect();
    assert!(
        !popover_quads.is_empty(),
        "popover background should be emitted at y={popover_top_y} (directly below header)",
    );

    // 2. Popover background must be the LAST quad-band — no other quad
    //    is emitted AFTER the popover with a higher index (z-order).
    let last_popover_idx = dl
        .quads
        .iter()
        .enumerate()
        .filter(|(_, q)| (q.rect.y - popover_top_y).abs() < 0.01)
        .map(|(i, _)| i)
        .max()
        .unwrap();
    // Allow the optional selected-row highlight + option-row quads
    // emitted *after* the background. They themselves count as popover
    // overlay quads, so the only valid quads after `last_popover_idx`
    // are also popover quads (y ≥ popover_top_y, y < popover bottom).
    let popover_bottom = popover_top_y + rect.h * options.len() as f32;
    for (i, q) in dl.quads.iter().enumerate() {
        if i <= last_popover_idx {
            continue;
        }
        assert!(
            q.rect.y >= popover_top_y - 0.01 && q.rect.y < popover_bottom + 0.01,
            "non-popover quad #{i} at y={} emitted AFTER popover (would overlay it)",
            q.rect.y,
        );
    }

    // 3. One text entry per option, each containing the option's label.
    for opt in &options {
        assert!(
            dl.texts.iter().any(|t| &t.text == opt),
            "popover should emit text for option {opt:?}",
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
    // leak.
    use sonic_shared::ui_tokens::UiPalette;

    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    s.set_category(Category::Appearance);
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

    let leaked = dl.quads.iter().any(|q| {
        (q.rect.y - popover_top_y).abs() < 0.01
            && (q.rect.x - rect.x).abs() < 0.01
            && q.color == palette.bg_hover
    });
    assert!(!leaked, "closed dropdown should not emit popover selected-row highlight");
}
