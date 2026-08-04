//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::pty::ShellSpawnOpts;

#[test]
fn exports_pty_spawn_options() {
    let opts = ShellSpawnOpts::default();
    assert!(!opts.clean_e2e);
}

#[test]
fn windows_process_snapshot_uses_aligned_storage() {
    const SOURCE: &str = include_str!("foreground_proc.rs");
    assert!(SOURCE.contains("let mut buf: Vec<u64>"));
    assert!(
        SOURCE.contains("align_of::<u64>() >= std::mem::align_of::<SystemProcessInformation>()")
    );
    assert!(!SOURCE.contains("let mut buf: Vec<u8>"));
}
