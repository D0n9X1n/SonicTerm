use config::{FontStretch, FontStyle, RgbaColor};

#[test]
fn exports_font_config_value_types() {
    assert_eq!(RgbaColor::default().alpha, 255);
    assert_eq!(FontStyle::Italic.to_string(), "Italic");
    assert_eq!(FontStretch::Normal.to_opentype_stretch(), 5);
}
