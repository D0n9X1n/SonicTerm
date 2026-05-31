//! Regression for Haiku review on PR #409.
//!
//! `GpuRenderer` caches `appearance.scrollbar` at construction, so a
//! live config reload must explicitly push a changed scrollbar policy to
//! every existing renderer. We cannot build a live `GpuRenderer` + wgpu
//! surface in this headless integration test; instead this pins the two
//! pure invariants the live path relies on:
//!
//! 1. the App-level config diff detects `Always -> Never`, and
//! 2. the same emit helper used by `GpuRenderer::render()` emits quads
//!    before the reload and no quads after the cached mode is updated.

use sonic_app::app::renderer_scrollbar_mode_differs;
use sonic_core::config::{Config, ScrollbarMode};
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_gpu::quad::QuadInstance;
use sonic_shared::render::emit_pane_scrollbar;
use sonic_ui::pane::Rect as PaneRect;

fn hex(s: &str) -> Hex {
    Hex(s.to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex("#000000"),
        red: hex("#111111"),
        green: hex("#222222"),
        yellow: hex("#333333"),
        blue: hex("#444444"),
        magenta: hex("#555555"),
        cyan: hex("#666666"),
        white: hex("#777777"),
    }
}

fn test_theme() -> Theme {
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex("#000000"),
            foreground: hex("#ffffff"),
            cursor: hex("#ffffff"),
            cursor_text: hex("#000000"),
            selection_bg: hex("#222222"),
            selection_fg: hex("#ffffff"),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex("#000000"),
                active_bg: hex("#111111"),
                active_fg: hex("#ffffff"),
                inactive_bg: hex("#000000"),
                inactive_fg: hex("#777777"),
                hover_bg: hex("#222222"),
                hover_fg: hex("#ffffff"),
                close_button_fg: hex("#777777"),
            },
        },
    }
}

fn emitted_scrollbar_quads(mode: ScrollbarMode) -> usize {
    let mut quads: Vec<QuadInstance> = Vec::new();
    emit_pane_scrollbar(
        &mut quads,
        PaneRect::new(0.0, 0.0, 800.0, 600.0),
        /* viewport_rows */ 24,
        /* total_rows    */ 10_000,
        /* view_top      */ 0,
        mode,
        &test_theme(),
        /* sw */ 800.0,
        /* sh */ 600.0,
    );
    quads.len()
}

#[test]
fn app_config_apply_pushes_scrollbar_mode_to_main_and_child_renderers() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/app/config_apply.rs"))
            .expect("read app/config_apply.rs");

    let start = src
        .find("if renderer_scrollbar_mode_differs(&self.config, &new_cfg)")
        .expect("scrollbar live-reload diff block must exist");
    let end = src[start..]
        .find("// Tab close-button override")
        .expect("scrollbar block should end before tab close-button block");
    let block = &src[start..start + end];

    assert!(
        block.contains("self.main_renderer_mut()"),
        "appearance.scrollbar reload must update the main renderer",
    );
    assert!(
        block.contains("self.windows.values_mut()"),
        "appearance.scrollbar reload must update child/torn-out renderers too",
    );
    assert_eq!(
        block.matches("r.set_scrollbar_mode(new_cfg.appearance.scrollbar)").count(),
        2,
        "apply_new_config must push the new cached scrollbar mode to both \
         the main renderer and every child renderer",
    );
}

#[test]
fn scrollbar_live_reload_always_to_never_suppresses_next_frame_emit() {
    let mut before = Config::default();
    before.appearance.scrollbar = ScrollbarMode::Always;
    let mut after = before.clone();
    after.appearance.scrollbar = ScrollbarMode::Never;

    assert!(
        renderer_scrollbar_mode_differs(&before, &after),
        "App live-reload diff must detect appearance.scrollbar changes so it calls \
         GpuRenderer::set_scrollbar_mode on existing renderers",
    );

    assert_eq!(
        emitted_scrollbar_quads(before.appearance.scrollbar),
        2,
        "pre-reload Always mode should emit track + thumb for a scrollable pane",
    );
    assert_eq!(
        emitted_scrollbar_quads(after.appearance.scrollbar),
        0,
        "post-reload Never mode must emit no scrollbar quads on the next frame",
    );
}
