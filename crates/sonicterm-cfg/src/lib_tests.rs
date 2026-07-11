//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::config::Config;

#[test]
fn exports_default_config_surface() {
    let cfg = Config::default();
    assert!(!cfg.theme.is_empty());
    assert!(!cfg.keymap.is_empty());
    assert!(cfg.window.cols > 0);
    assert!(cfg.window.rows > 0);
}
