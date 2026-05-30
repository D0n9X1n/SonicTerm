//! `Config::quit_on_last_window_close` field.
//!
//! Default is `true` (traditional terminal behavior: closing the last
//! window quits the app). Users who want Chrome/Firefox/Safari-style
//! dock-alive behavior set `quit_on_last_window_close = false` in
//! `sonic.toml`. Flip rationale: user testing showed Cmd+W on the last
//! tab "did nothing" — the discoverability cost of a dock-alive default
//! outweighed the cold-start cost it was meant to avoid.

use sonic_cfg::config::Config;

#[test]
fn default_quit_on_last_window_close_is_true_traditional_terminal() {
    let cfg = Config::default();
    assert!(
        cfg.quit_on_last_window_close,
        "default must be true (traditional terminal): closing the last \
         window quits the app. Users who want dock-alive set \
         quit_on_last_window_close = false explicitly."
    );
}

#[test]
fn empty_toml_yields_true_for_quit_on_last_window_close() {
    let cfg: Config = toml::from_str("").expect("empty TOML parses as Config");
    assert!(cfg.quit_on_last_window_close);
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
