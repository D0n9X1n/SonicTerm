//! Regression test for issue #167:
//!
//! Changing `font.size` in the prefs UI (Cmd/Ctrl+,) had no live
//! effect on the terminal - the new size was persisted to disk and
//! mirrored into `self.config`, but the renderer's `set_font(...)`
//! was never called, so the on-screen text kept rendering at the old
//! size until the user manually triggered a full restart.
//!
//! Root cause: `commit_prefs_and_apply_live` had explicit fast-paths
//! for theme + keymap reloads, but mirrored every other field via
//! `self.config = s.config.clone()`. When the config-watcher's
//! debounced `apply_new_config(latest)` later fired, the diff against
//! the (already-updated) live config was a no-op, so the font path
//! never ran.
//!
//! The fix routes prefs Apply through `apply_new_config`, the
//! canonical live-reload path. The single observable signal that
//! distinguishes the fixed code from the regression - without
//! needing a live wgpu surface - is that `apply_new_config` sets
//! `App::input_dirty = true` (PR #132: any user-driven live reload
//! must render immediately, not at the next vsync deadline). The
//! pre-fix code path mirrored `self.config` directly and never
//! invoked `apply_new_config`, so `input_dirty` stayed `false` even
//! though the config field updated.
//!
//! Bisect verified: reverting only `crates/sonic-app/src/app/
//! prefs_window.rs` to `origin/main` makes
//! `font_size_apply_through_prefs_marks_input_dirty` FAIL on the
//! `input_dirty` assertion (config still updates via the old
//! self.config-mirror path, but the renderer-driving flag never
//! flips).

use sonic_app::app::{config_diff_needs_font_apply, App};
use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_ui::prefs::PrefsState;
use std::path::PathBuf;

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

fn synth_theme() -> Theme {
    Theme {
        name: "test".to_string(),
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

fn temp_config_path() -> PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "sonic-prefs-test-{}-{}.toml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    p.push(unique);
    p
}

#[test]
fn font_size_change_is_detected_as_needing_live_apply() {
    let mut old = Config::default();
    old.font.size = 14.0;
    let mut new = Config::default();
    new.font.size = 24.0;
    assert!(
        config_diff_needs_font_apply(&old, &new),
        "font.size change must mark the renderer's font path as dirty"
    );
}

#[test]
fn unchanged_font_is_not_re_applied() {
    let old = Config::default();
    let new = Config::default();
    assert!(
        !config_diff_needs_font_apply(&old, &new),
        "identical font block must not trigger a needless re-apply"
    );
}

#[test]
fn font_family_change_is_detected() {
    let mut old = Config::default();
    old.font.family = "JetBrains Mono".to_string();
    let mut new = Config::default();
    new.font.family = "Fira Code".to_string();
    assert!(config_diff_needs_font_apply(&old, &new));
}

#[test]
fn font_line_height_change_is_detected() {
    let mut old = Config::default();
    old.font.line_height = 1.0;
    let mut new = Config::default();
    new.font.line_height = 1.4;
    assert!(config_diff_needs_font_apply(&old, &new));
}

// The actual regression test for issue #167.
// Drives the SAME function (commit_prefs_and_apply_live) that the
// prefs UI invokes when the user clicks Apply.
#[test]
fn font_size_apply_through_prefs_marks_input_dirty() {
    let mut cfg = Config::default();
    cfg.font.size = 14.0;
    let mut app = App::new(synth_theme(), cfg.clone(), empty_keymap());

    assert!(!app.input_dirty_for_test(), "fresh App must start with input_dirty=false");
    assert!(
        (app.font_size_for_test() - 14.0).abs() < f32::EPSILON,
        "fresh App must reflect cfg.font.size"
    );

    let path = temp_config_path();
    let mut prefs = PrefsState::new(cfg.clone(), path.clone(), synth_theme());
    prefs.config.font.size = 24.0;
    prefs.dirty = true;
    app.install_prefs_state_for_test(prefs);

    app.commit_prefs_for_test();

    // Bisect signal: pre-fix mirrored self.config directly without going
    // through apply_new_config, so input_dirty never flipped.
    assert!(
        app.input_dirty_for_test(),
        "commit_prefs_and_apply_live must route through apply_new_config so the          renderer-driving input_dirty flag is set; without this the user's font          change sits invisible until the next unrelated input event (issue #167)"
    );

    assert!(
        (app.font_size_for_test() - 24.0).abs() < f32::EPSILON,
        "font.size must reach the live App config"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_op_prefs_commit_does_not_mark_input_dirty() {
    let cfg = Config::default();
    let mut app = App::new(synth_theme(), cfg.clone(), empty_keymap());
    app.clear_input_dirty_for_test();

    let path = temp_config_path();
    let prefs = PrefsState::new(cfg, path, synth_theme());
    app.install_prefs_state_for_test(prefs);

    app.commit_prefs_for_test();

    assert!(
        !app.input_dirty_for_test(),
        "a non-dirty prefs commit must short-circuit and not poke the renderer"
    );
}
