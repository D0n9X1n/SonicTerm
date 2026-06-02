//! Epic #289 Phase E — last-window-close behavior is gated on
//! `Config::quit_on_last_window_close` and the host platform.
//!
//! These tests exercise `App::should_exit_on_last_window_close`
//! directly; the actual `el.exit()` call site lives behind a real
//! winit event loop, but the predicate is the testable seam — the
//! call-site is a one-liner that forwards to it.

use sonicterm_app::app::App;
use sonicterm_cfg::config::Config;

#[cfg(target_os = "macos")]
#[test]
fn macos_default_config_exits_on_last_window_close_traditional_terminal() {
    // Traditional terminal behavior: closing the last window quits the
    // app. Flip rationale: user testing showed Cmd+W on the last tab
    // "did nothing" — the discoverability cost of dock-alive default
    // outweighed the cold-start cost it was meant to avoid. Chrome-style
    // remains available via `quit_on_last_window_close = false`.
    let cfg = Config::default();
    assert!(cfg.quit_on_last_window_close, "default must be true");
    assert!(
        App::should_exit_on_last_window_close(&cfg),
        "macOS + default config (true) → MUST exit on last window close"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_explicit_false_keeps_app_alive_chrome_mode() {
    // Opt-in Chrome/Firefox/Safari-style dock-alive on macOS.
    let cfg = Config { quit_on_last_window_close: false, ..Config::default() };
    assert!(
        !App::should_exit_on_last_window_close(&cfg),
        "macOS + quit_on_last_window_close=false → must NOT exit on last window close"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_default_config_exits_on_last_window_close() {
    // Windows/Linux have no dock concept; the app process exiting
    // when the last window closes is the user-expected behavior.
    let cfg = Config::default();
    assert!(
        App::should_exit_on_last_window_close(&cfg),
        "non-macOS + default config → MUST exit on last window close \
         regardless of quit_on_last_window_close value"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_ignores_quit_on_last_window_close_false() {
    // On non-macOS the config is ignored: setting it to false does
    // NOT pin the process alive (there's nowhere for it to live —
    // no dock, no menubar app affordance).
    let cfg = Config { quit_on_last_window_close: false, ..Config::default() };
    assert!(
        App::should_exit_on_last_window_close(&cfg),
        "non-macOS ignores the config and always exits"
    );
}
