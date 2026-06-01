//! #386 PR-B render-emit test.
//!
//! Drives the public `emit_pane_scrollbar` helper directly. The helper
//! is doc-hidden but `pub` for exactly this test surface — we can't spin
//! up a full `GpuRenderer::render` call without a wgpu surface, but the
//! quad-emission path is the only thing PR-B actually adds, so verifying
//! it in isolation pins the regression bar.
//!
//! Three claims:
//! 1. A pane with scrollback > viewport emits exactly 2 quads (track + thumb).
//! 2. A pane with total <= viewport emits 0 quads.
//! 3. `ScrollbarMode::Never` emits 0 quads regardless of scrollback.

use sonicterm_cfg::config::ScrollbarMode;
use sonicterm_cfg::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonicterm_gpu::quad::QuadInstance;
use sonicterm_shared::render::emit_pane_scrollbar;
use sonicterm_ui::pane::Rect as PaneRect;

fn test_theme() -> Theme {
    let h = || Hex("#000000".to_string());
    let one = || Hex("#ffffff".to_string());
    let ansi = AnsiColors {
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
            foreground: one(),
            cursor: one(),
            cursor_text: h(),
            selection_bg: h(),
            selection_fg: one(),
            ansi: ansi.clone(),
            bright: ansi,
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
fn emits_track_and_thumb_when_scrollable() {
    let mut quads: Vec<QuadInstance> = Vec::new();
    let pane = PaneRect::new(0.0, 0.0, 800.0, 600.0);
    let n = emit_pane_scrollbar(
        &mut quads,
        pane,
        /* viewport_rows */ 24,
        /* total_rows    */ 10_000,
        /* view_top      */ 0,
        ScrollbarMode::Auto,
        &test_theme(),
        /* sw */ 800.0,
        /* sh */ 600.0,
        /* alpha */ 1.0,
    );
    assert_eq!(n, 2, "expected 2 quads (track + thumb), got {n}");
    assert_eq!(quads.len(), 2);
    // Track and thumb must differ in alpha (thumb > track) so the
    // hierarchy reads correctly on screen.
    let track_alpha = quads[0].color[3];
    let thumb_alpha = quads[1].color[3];
    assert!(
        thumb_alpha > track_alpha,
        "thumb alpha {thumb_alpha} should exceed track alpha {track_alpha}"
    );
}

#[test]
fn no_emit_when_total_le_viewport() {
    let mut quads: Vec<QuadInstance> = Vec::new();
    let pane = PaneRect::new(0.0, 0.0, 800.0, 600.0);
    let n = emit_pane_scrollbar(
        &mut quads,
        pane,
        /* viewport_rows */ 24,
        /* total_rows    */ 24, // no scrollback
        /* view_top      */ 0,
        ScrollbarMode::Auto,
        &test_theme(),
        800.0,
        600.0,
        1.0,
    );
    assert_eq!(n, 0);
    assert!(quads.is_empty());
}

#[test]
fn no_emit_when_mode_never() {
    let mut quads: Vec<QuadInstance> = Vec::new();
    let pane = PaneRect::new(0.0, 0.0, 800.0, 600.0);
    let n = emit_pane_scrollbar(
        &mut quads,
        pane,
        24,
        10_000,
        0,
        ScrollbarMode::Never,
        &test_theme(),
        800.0,
        600.0,
        1.0,
    );
    assert_eq!(n, 0);
    assert!(quads.is_empty());
}

#[test]
fn always_mode_also_emits_when_scrollable() {
    let mut quads: Vec<QuadInstance> = Vec::new();
    let pane = PaneRect::new(0.0, 0.0, 800.0, 600.0);
    let n = emit_pane_scrollbar(
        &mut quads,
        pane,
        24,
        10_000,
        0,
        ScrollbarMode::Always,
        &test_theme(),
        800.0,
        600.0,
        1.0,
    );
    assert_eq!(n, 2);
}
