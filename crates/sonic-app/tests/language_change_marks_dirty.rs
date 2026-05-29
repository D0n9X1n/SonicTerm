//! Regression test for issue #253:
//!
//! Changing the UI language in prefs persisted to disk but did not update
//! the live app-level i18n bundle, so UI strings stayed in the old language
//! until restart. Applying prefs must mark `input_dirty` and switch
//! `App::locale()` immediately.

use sonic_app::app::App;
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
        "sonic-prefs-language-test-{}-{}.toml",
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
fn language_change_apply_through_prefs_marks_input_dirty_and_switches_locale() {
    let cfg = Config { locale: "en".to_string(), ..Default::default() };
    let mut app = App::new(synth_theme(), cfg.clone(), empty_keymap());

    assert!(!app.input_dirty_for_test(), "fresh App must start with input_dirty=false");
    assert_eq!(app.locale(), "en", "fresh App must reflect cfg.locale");

    let path = temp_config_path();
    let mut prefs = PrefsState::new(cfg, path.clone(), synth_theme());
    prefs.config.locale = "zh-CN".to_string();
    prefs.dirty = true;
    app.install_prefs_state_for_test(prefs);

    app.commit_prefs_for_test();

    assert!(
        app.input_dirty_for_test(),
        "commit_prefs_and_apply_live must route language changes through apply_new_config so the renderer-driving input_dirty flag is set"
    );
    assert_eq!(app.locale(), "zh-CN", "locale must reach the live App i18n bundle");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_op_prefs_commit_does_not_mark_input_dirty() {
    let cfg = Config::default();
    let mut app = App::new(synth_theme(), cfg.clone(), empty_keymap());
    app.clear_input_dirty_for_test();

    let path = temp_config_path();
    let prefs = PrefsState::new(cfg, path.clone(), synth_theme());
    app.install_prefs_state_for_test(prefs);

    app.commit_prefs_for_test();

    assert!(
        !app.input_dirty_for_test(),
        "a non-dirty prefs commit must short-circuit and not poke the renderer"
    );

    let _ = std::fs::remove_file(&path);
}
