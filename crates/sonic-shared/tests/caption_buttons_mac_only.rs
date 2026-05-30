#[test]
fn caption_buttons_are_only_painted_on_macos() {
    let source = include_str!("../src/render/core.rs");
    let cfg_idx = source.find("#[cfg(target_os = \"macos\")]").expect("macOS cfg guard exists");
    let paint_idx = source
        .find("crate::quad::paint_caption_buttons")
        .expect("caption button paint call exists");
    assert!(
        cfg_idx < paint_idx,
        "paint_caption_buttons must be cfg(macos) so Windows native chrome is not duplicated",
    );
}
