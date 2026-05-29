//! Epic #289 Phase E — `quit_on_last_window_close` config field.
//!
//! Default must be `false` (Chrome/Firefox/Safari-style on macOS:
//! keep app alive after the last window closes so the user can
//! `Cmd+N` to open a fresh window from the dock without paying
//! cold-start cost). An explicit `true` in `sonic.toml` opts back
//! into classic single-window-app behavior.

use sonic_cfg::config::Config;

#[test]
fn default_quit_on_last_window_close_is_false_chrome_style() {
    let cfg = Config::default();
    assert!(
        !cfg.quit_on_last_window_close,
        "default must be false (Chrome/Firefox/Safari-style): app should \
         stay alive after the last window closes on macOS"
    );
}

#[test]
fn empty_toml_yields_false_for_quit_on_last_window_close() {
    let cfg: Config = toml::from_str("").expect("empty TOML parses as Config");
    assert!(!cfg.quit_on_last_window_close);
}

#[test]
fn explicit_quit_on_last_window_close_true_parses() {
    let cfg: Config = toml::from_str("quit_on_last_window_close = true\n").expect("toml parses");
    assert!(cfg.quit_on_last_window_close);
}

#[test]
fn explicit_quit_on_last_window_close_false_parses() {
    let cfg: Config = toml::from_str("quit_on_last_window_close = false\n").expect("toml parses");
    assert!(!cfg.quit_on_last_window_close);
}
