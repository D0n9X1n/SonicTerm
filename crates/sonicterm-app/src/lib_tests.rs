//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::shell::LinuxShell;
use crate::{KeymapLoader, ProcessPrivilege, ThemeLoader};

#[test]
fn exports_loader_type_aliases() {
    let _: Option<KeymapLoader> = None;
    let _: Option<ThemeLoader> = None;
}

#[test]
fn exports_linux_platform_shell() {
    // Protect the platform-neutral app runner surface consumed by the Linux binary crate.
    let _: Option<LinuxShell> = None;
}

#[test]
fn exports_process_privilege_contract() {
    // Protect platform binaries from replacing the typed process-level state with title inference.
    assert!(!ProcessPrivilege::default().is_privileged());
    assert!(ProcessPrivilege::Privileged.is_privileged());
}

#[test]
fn headless_app_defaults_to_unprivileged() {
    // Protect existing and headless constructors from painting an unobserved privilege warning.
    let app = crate::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );

    assert_eq!(app.process_privilege(), ProcessPrivilege::Unprivileged);
}
