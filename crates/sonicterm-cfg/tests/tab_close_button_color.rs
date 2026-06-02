//! Regression for PR #96 review:
//!
//! 1. `tab_close_button_color` set in sonicterm.toml must reach the
//!    application config at startup (so the renderer-init code path
//!    in `App` can apply it BEFORE the first frame, not on the first
//!    live-reload).
//! 2. A live-reload that introduces or changes the value must be
//!    observable through plain `Config` equality so the
//!    `apply_new_config` diff in `App` fires the renderer push.
//!
//! These tests deliberately drive `sonicterm_cfg::config::Config` rather
//! than `GpuRenderer` — the renderer requires a live winit window +
//! wgpu adapter, which is unavailable in CI. The App's startup +
//! live-reload paths only consume `Config::tab_close_button_color`,
//! so verifying the parser + diff is the meaningful coverage.

use sonicterm_cfg::config::Config;
use std::fs;
use tempfile::TempDir;

#[test]
fn startup_with_custom_tab_close_button_color_parses_into_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonicterm.toml");
    fs::write(
        &path,
        r##"
theme = "tokyo-night"
keymap = "sonicterm"
tab_close_button_color = "#ff5555"

[font]
family = "Rec Mono St.Helens"
size = 14.0
line_height = 1.2
"##,
    )
    .unwrap();
    let cfg = Config::load_strict(&path).unwrap();
    // The value the App reads at startup must already contain the
    // user's override — without it the renderer would init with the
    // default (None) and the × would only become always-visible after
    // the next config write triggered the live-reload diff.
    assert_eq!(cfg.tab_close_button_color.as_deref(), Some("#ff5555"));
}

#[test]
fn live_reload_change_to_tab_close_button_color_is_observable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonicterm.toml");
    fs::write(
        &path,
        r##"
theme = "tokyo-night"
keymap = "sonicterm"

[font]
family = "Rec Mono St.Helens"
size = 14.0
line_height = 1.2
"##,
    )
    .unwrap();
    let before = Config::load_strict(&path).unwrap();
    assert_eq!(before.tab_close_button_color, None);

    fs::write(
        &path,
        r##"
theme = "tokyo-night"
keymap = "sonicterm"
tab_close_button_color = "#aabbcc"

[font]
family = "Rec Mono St.Helens"
size = 14.0
line_height = 1.2
"##,
    )
    .unwrap();
    let after = Config::load_strict(&path).unwrap();
    // `apply_new_config` diffs old vs new with `!=` on this field;
    // the test mirrors that exact comparison so a future refactor
    // that, say, normalizes the string at parse-time and breaks the
    // diff will be caught here.
    assert_ne!(before.tab_close_button_color, after.tab_close_button_color);
    assert_eq!(after.tab_close_button_color.as_deref(), Some("#aabbcc"));
}
