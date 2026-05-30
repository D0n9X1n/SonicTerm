//! `quit_on_last_window_close` config field.
//!
//! Default is `true` (traditional terminal-emulator behavior:
//! Terminal.app, iTerm2, Alacritty, WezTerm — closing the last
//! tab/window quits the app). Users who prefer the
//! Chrome/Firefox/Safari dock-alive style on macOS can opt in by
//! setting `quit_on_last_window_close = false` in `sonic.toml`.

use sonic_cfg::config::Config;

#[test]
fn default_quit_on_last_window_close_is_true_traditional_terminal_style() {
    let cfg = Config::default();
    assert!(
        cfg.quit_on_last_window_close,
        "default must be true (traditional terminal behavior): closing the \
         last tab/window must quit the app, matching Terminal.app / iTerm2 \
         / Alacritty / WezTerm"
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
