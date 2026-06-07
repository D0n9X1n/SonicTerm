use sonicterm_gpu::color::{chrome_color_to_linear_rgba, ChromeColor};

#[test]
fn exports_color_conversion_helpers() {
    let rgba = chrome_color_to_linear_rgba(ChromeColor::rgb(255, 0, 0));
    assert_eq!(rgba[0], 1.0);
    assert_eq!(rgba[1], 0.0);
    assert_eq!(rgba[2], 0.0);
    assert_eq!(rgba[3], 1.0);
}
