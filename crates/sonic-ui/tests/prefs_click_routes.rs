//! Regression for macOS user bug: "Cmd+, opens preferences but clicking
//! ANYTHING in the window does nothing" (sidebar items don't switch,
//! buttons don't fire, dropdowns don't open).
//!
//! Root cause was a coordinate-space mismatch at the dispatcher level:
//! `WindowEvent::CursorMoved`/`MouseInput` report positions in
//! **physical** pixels, but [`PrefsState`] / [`PrefsLayout`] are
//! authored in **logical** pixels (see `prefs/layout.rs` module doc —
//! "All numbers are in *logical* pixels"). On any Retina display
//! (scale_factor = 2.0) the dispatcher passed raw physical coordinates
//! into `classify_click`, which then mapped every click to a position
//! ~2× the intended one and fell off every widget rect.
//!
//! The dispatcher fix lives in `crates/sonic-app/src/app/prefs_window.rs`
//! (the press, release, and cursor-moved arms now all run
//! `to_logical_pos(self.cursor_pos.*, prefs_window.scale_factor())`).
//! These tests pin the contract at the [`PrefsState`] level:
//!
//! 1. A click at the **logical** center of `apply_button` produces
//!    `PrefsHit::Apply`. (Round-trip sanity.)
//! 2. A click at the **physical-pixel equivalent** of that center on a
//!    2× display does NOT hit Apply (or anything else), proving the
//!    dispatcher MUST convert before forwarding.
//! 3. A click at the logical center of the first sidebar category row
//!    produces `PrefsHit::Sidebar(...)` (regression for the visible
//!    user symptom: "sidebar items don't switch sections").
//!
//! If you find yourself updating these tests because the dispatcher
//! stopped converting, you are re-introducing the bug — fix the
//! dispatcher instead.

use sonic_cfg::config::Config;
use sonic_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_ui::prefs::{layout::Category, PrefsHit, PrefsState};
use std::path::PathBuf;

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
                inactive_bg: h(),
                active_fg: h(),
                inactive_fg: h(),
                hover_bg: h(),
                hover_fg: h(),
                close_button_fg: h(),
            },
        },
    }
}

fn state() -> PrefsState {
    PrefsState::new(Config::default(), PathBuf::from("ignored.toml"), theme())
}

#[test]
fn apply_button_hit_at_logical_center() {
    let s = state();
    let r = s.layout.apply_button;
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert!(matches!(s.classify_click(cx, cy), Some(PrefsHit::Apply)));
}

#[test]
fn apply_button_miss_at_2x_physical_coords() {
    // This is exactly what the buggy dispatcher fed into classify_click
    // on a Retina display: the raw physical-pixel position. If anyone
    // re-introduces the bug by removing the to_logical_pos() call in
    // prefs_window.rs, the dispatcher will pass these doubled coords
    // and classify_click will return None — every click silently lost.
    let s = state();
    let r = s.layout.apply_button;
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let (phys_x, phys_y) = (cx * 2.0, cy * 2.0);
    assert!(
        s.classify_click(phys_x, phys_y).is_none(),
        "physical-pixel coords on 2x display must miss the button — \
         dispatcher is responsible for the logical conversion (see \
         to_logical_pos in prefs_window.rs)",
    );
}

#[test]
fn sidebar_first_category_hit_at_logical_center() {
    let s = state();
    let row = s.layout.category_row(0);
    let (cx, cy) = (row.x + row.w / 2.0, row.y + row.h / 2.0);
    match s.classify_click(cx, cy) {
        Some(PrefsHit::Sidebar(cat)) => {
            // First row is Font (matches Category list order in
            // prefs/layout.rs); be tolerant if the order is reshuffled
            // later — what matters is that *some* sidebar hit fires.
            let _ = cat;
        }
        other => panic!("expected Sidebar hit at first category row, got {other:?}"),
    }
    // Suppress "Category unused" warning on tolerant matches.
    let _ = Category::Font;
}

#[test]
fn cancel_button_hit_at_logical_center() {
    let s = state();
    let r = s.layout.cancel_button;
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    assert!(matches!(s.classify_click(cx, cy), Some(PrefsHit::Cancel)));
}
