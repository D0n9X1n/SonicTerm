use sonic_text::swash_rasterizer::monochrome_render_config_for_test;
use swash::scale::Source;
use swash::zeno::Format;

#[test]
fn monochrome_rasterizer_uses_hinted_outlines() {
    let (sources, format, hint) = monochrome_render_config_for_test();
    assert!(hint, "outline scaler must enable hinting");
    assert_eq!(
        format,
        Format::Alpha,
        "all platforms use grayscale alpha masks until the Windows LCD integration is fixed (#316)"
    );
    assert!(
        sources.iter().any(|source| matches!(source, Source::Outline)),
        "monochrome rasterizer must include outline source"
    );
}
