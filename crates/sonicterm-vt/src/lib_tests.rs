//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

/// The exported XTVERSION identity is derived from the crate's release version,
/// not a hand-maintained literal that can fall behind releases.
#[test]
fn exports_version_string() {
    assert_eq!(crate::vt::SONIC_VERSION, concat!("SonicTerm ", env!("CARGO_PKG_VERSION")));
}
