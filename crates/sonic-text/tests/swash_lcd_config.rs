use sonic_text::swash_rasterizer::monochrome_render_config_for_test;
use swash::scale::Source;
use swash::zeno::Format;

#[test]
fn monochrome_rasterizer_uses_hinted_outlines() {
    let (sources, format, hint) = monochrome_render_config_for_test();
    assert!(hint, "outline scaler must enable hinting");
    #[cfg(target_os = "windows")]
    assert_eq!(
        format,
        Format::Subpixel,
        "Windows uses LCD subpixel masks (ClearType parity, #261)"
    );
    #[cfg(not(target_os = "windows"))]
    assert_eq!(
        format,
        Format::Alpha,
        "macOS + Linux use grayscale alpha masks; LCD subpixel produces color fringing on macOS"
    );
    assert!(
        sources.iter().any(|source| matches!(source, Source::Outline)),
        "monochrome rasterizer must include outline source"
    );
}
