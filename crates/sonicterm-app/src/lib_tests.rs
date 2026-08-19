//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::shell::LinuxShell;
use crate::{KeymapLoader, ThemeLoader};

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
