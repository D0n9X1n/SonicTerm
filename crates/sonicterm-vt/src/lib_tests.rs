//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

#[test]
fn exports_version_string() {
    assert!(crate::vt::SONIC_VERSION.starts_with("SonicTerm "));
}
