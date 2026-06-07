#[test]
fn unit_test_target_is_present() {
    assert_eq!(env!("CARGO_PKG_NAME"), "sonicterm-mac");
}
