//! Unit tests for the `proc_info` foreground-process probe.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/proc_info.rs`
//! per the per-crate `tests/` layout convention (CLAUDE.md §5).

use sonicterm_io::proc_info::normalize_proc_name;

#[test]
fn strips_login_dash_prefix() {
    assert_eq!(normalize_proc_name("-zsh"), "zsh");
}

#[test]
fn extracts_basename_from_path() {
    assert_eq!(normalize_proc_name("/usr/local/bin/nvim"), "nvim");
}

#[test]
fn lowercases() {
    assert_eq!(normalize_proc_name("Python"), "python");
}

#[test]
fn handles_plain_name() {
    assert_eq!(normalize_proc_name("bash"), "bash");
}
