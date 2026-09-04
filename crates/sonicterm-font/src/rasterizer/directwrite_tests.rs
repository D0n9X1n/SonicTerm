use super::*;

/// DirectWrite coverage stays native when the configured weight scale is identity.
#[test]
fn directwrite_coverage_preserves_native_channels() {
    assert_eq!(directwrite_coverage_pixel([0, 128, 255]), [0, 128, 255, 255]);
    assert_eq!(directwrite_coverage_pixel([1, 64, 254]), [1, 64, 254, 254]);
}
