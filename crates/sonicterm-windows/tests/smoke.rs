#[test]
fn integration_test_target_is_present() {
    assert_eq!(env!("CARGO_PKG_NAME"), "sonicterm-windows");
}
