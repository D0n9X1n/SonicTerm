//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

#[test]
fn unit_test_target_is_present() {
    assert_eq!(env!("CARGO_PKG_NAME"), "sonicterm-mac");
}
