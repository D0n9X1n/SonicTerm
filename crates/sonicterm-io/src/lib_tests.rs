//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::pty::ShellSpawnOpts;

#[test]
fn exports_pty_spawn_options() {
    let opts = ShellSpawnOpts::default();
    assert!(!opts.clean_e2e);
}
