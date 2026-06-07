#[test]
fn exports_version_string() {
    assert!(sonicterm_vt::vt::SONIC_VERSION.starts_with("SonicTerm "));
}
