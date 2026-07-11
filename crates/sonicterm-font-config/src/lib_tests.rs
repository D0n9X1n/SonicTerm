//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{FontStretch, FontStyle, RgbaColor};

#[test]
fn exports_font_config_value_types() {
    assert_eq!(RgbaColor::default().alpha, 255);
    assert_eq!(FontStyle::Italic.to_string(), "Italic");
    assert_eq!(FontStretch::Normal.to_opentype_stretch(), 5);
}
