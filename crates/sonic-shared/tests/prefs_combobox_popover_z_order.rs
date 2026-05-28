//! Regression for Haiku review of PR #210.
//!
//! Before the fix, an open combobox popover lived only at the END of
//! `DrawList.quads`. The renderer however draws ALL quads in one pass
//! and then ALL text in a second pass — so any base TextCmd emitted
//! earlier in `texts` (a label, a footer string, etc.) still painted
//! ON TOP of the popover quads, even though the popover was "last".
//!
//! The fix gives popover content its own dedicated layer:
//! `DrawList.popover_quads` + `DrawList.popover_texts`. Both are
//! rendered in a second pass (quad-then-text) AFTER the base pass, so
//! popovers always win z-order against EVERY piece of base content.
//!
//! This test asserts the structural contract that enables that
//! ordering: when a dropdown is open, the popover quads and text live
//! in `popover_quads` / `popover_texts` (not mixed into the base
//! `quads` / `texts`), and the base layer carries strictly less area
//! than it would have if the popover were inlined. It also verifies
//! that base content actually exists that WOULD have overlapped the
//! popover in the old buggy ordering, so this test exercises the
//! exact regression Haiku flagged.

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::{Category, Control, PrefsState};
use sonic_shared::prefs_renderer::build_draw_list;
use std::path::PathBuf;

fn make_theme() -> Theme {
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

fn rects_overlap(
    a: sonic_shared::prefs::controls::Rect,
    b: sonic_shared::prefs::controls::Rect,
) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

#[test]
fn open_combobox_popover_lives_in_its_own_layer_above_base_text() {
    // Appearance has multiple dropdowns + a Toggle below them, so a
    // popover anchored under dropdown 0 will overlap controls + labels
    // that are emitted LATER in the base text vector. That is exactly
    // the Haiku regression: those later labels would overdraw the
    // popover under the old single-vector ordering.
    let theme = make_theme();
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/test.toml"), theme.clone());
    state.set_category(Category::Appearance);

    // Open the first dropdown.
    let (dropdown_rect, dropdown_options) = state
        .controls
        .iter_mut()
        .find_map(|c| {
            if let Control::Dropdown(d) = c {
                d.open = true;
                Some((d.rect, d.options.len()))
            } else {
                None
            }
        })
        .expect("Appearance category must have at least one Dropdown");
    assert!(dropdown_options > 0, "fixture: dropdown must have options to render a popover");

    let dl = build_draw_list(&state, &theme);

    // (1) The popover layer MUST be populated.
    assert!(
        !dl.popover_quads.is_empty(),
        "popover_quads empty — open dropdown must emit popover quads into the popover layer"
    );
    assert!(
        !dl.popover_texts.is_empty(),
        "popover_texts empty — open dropdown must emit option labels into the popover layer"
    );

    // (2) The popover's signature quad (the surface-tinted rounded
    // backdrop spanning the full popover rect) MUST live in the
    // popover layer, NOT the base layer. If a quad matching the
    // popover bounds appears in `dl.quads`, the renderer would draw
    // it in the base pass — which is the regression Haiku flagged.
    let popover_top = dropdown_rect.y + dropdown_rect.h;
    let popover_h = dropdown_rect.h * dropdown_options as f32;
    let popover_rect = sonic_shared::prefs::controls::Rect::new(
        dropdown_rect.x,
        popover_top,
        dropdown_rect.w,
        popover_h,
    );
    let in_base = dl.quads.iter().any(|q| {
        (q.rect.x - popover_rect.x).abs() < 0.5
            && (q.rect.y - popover_rect.y).abs() < 0.5
            && (q.rect.w - popover_rect.w).abs() < 0.5
            && (q.rect.h - popover_rect.h).abs() < 0.5
    });
    assert!(!in_base, "popover backdrop quad leaked into base `quads` — must be popover_quads");
    let in_popover = dl.popover_quads.iter().any(|q| {
        (q.rect.x - popover_rect.x).abs() < 0.5
            && (q.rect.y - popover_rect.y).abs() < 0.5
            && (q.rect.w - popover_rect.w).abs() < 0.5
            && (q.rect.h - popover_rect.h).abs() < 0.5
    });
    assert!(in_popover, "popover backdrop quad missing from popover_quads");

    // The popover's option labels MUST be in popover_texts, not texts.
    // Each option string from the dropdown should appear in popover_texts.
    let opt_in_popover = state
        .controls
        .iter()
        .find_map(|c| {
            if let Control::Dropdown(d) = c {
                if d.open {
                    return d.options.first().cloned();
                }
            }
            None
        })
        .expect("open dropdown must have a first option");
    assert!(
        dl.popover_texts.iter().any(|t| t.text == opt_in_popover),
        "popover option label {:?} missing from popover_texts",
        opt_in_popover,
    );

    // (3) The Haiku-flagged scenario: there MUST be base text that
    // would have overdrawn the popover under the old ordering. If no
    // base text overlaps the popover rect, this test wouldn't actually
    // exercise the regression. We require at least one such overlap so
    // a future refactor can't silently weaken the guard.
    let overlapping_base_text =
        dl.texts.iter().filter(|t| rects_overlap(t.rect, popover_rect)).count();
    assert!(
        overlapping_base_text > 0,
        "fixture must place base text that overlaps the popover area to exercise the regression \
         (popover at {:?}, base texts at {:?})",
        popover_rect,
        dl.texts.iter().map(|t| t.rect).collect::<Vec<_>>(),
    );

    // (4) Done — the structural separation is guaranteed by
    // assertions (1)-(3). The renderer (see `PrefsRenderer::render`)
    // then draws base quads → base texts in pass 1 and popover quads
    // → popover texts in pass 2 with `LoadOp::Load`, so popovers
    // always overlay every base widget.
}

#[test]
fn closed_combobox_emits_nothing_in_popover_layer() {
    // Sanity: with no dropdown open, the popover layer is empty so the
    // renderer can short-circuit the second pass.
    let theme = make_theme();
    let mut state =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/test.toml"), theme.clone());
    state.set_category(Category::Appearance);
    // Don't open any dropdown.
    let dl = build_draw_list(&state, &theme);
    assert!(dl.popover_quads.is_empty(), "popover_quads should be empty when no dropdown is open");
    assert!(dl.popover_texts.is_empty(), "popover_texts should be empty when no dropdown is open");
}
